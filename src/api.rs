//! The S-ostoslista GraphQL API (AWS AppSync).
//!
//! AppSync serves two disjoint surfaces. User operations (lists, product
//! search, recommendations) authenticate with the bare access token in
//! `Authorization`. Product lookups by id or EAN are only open to the app's
//! public API key in `X-Api-Key`. A request carries one or the other: with
//! both, AppSync picks API-key auth and denies the user operations.

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::auth::Session;

pub const GRAPHQL_URL: &str = "https://api.s-ostoslista.fi/graphql";
/// The anonymous API key embedded in the S-ostoslista app.
const PUBLIC_API_KEY: &str = "da2-ivqewtjq3re3nnctbhouukg37m";
/// Base of the links the app opens to join a shared list.
pub const SHARE_URL: &str = "https://s-ostoslista.fi/liity/";

const LIST_FIELDS: &str = "
fragment ListFields on ShoppingList {
  id
  name
  modifiedAt
  shared
  storeId
  store { id name }
  items {
    id
    name
    collected
    ... on ProductResolvedShoppingListItem {
      ean
      quantity
      quantityUnit
      sokId
      product { pricing { currentPrice } location { aisle label } }
    }
  }
}";

/// Product fields for search results and recommendations.
const PRODUCT_FIELDS: &str = "
fragment ProductFields on Product {
  id
  ean
  name
  brandName
  available
  priceUnit
  basicQuantityUnit
  comparisonUnit
  approxPrice
  storeId
  pricing {
    currentPrice
    regularPrice
    comparisonPrice
    campaignPrice
    campaignPriceValidUntil
    lowest30DayPrice
    depositPrice
  }
  location { aisle floor label name }
}";

const PRODUCT_DETAIL_FIELDS: &str = "
fragment ProductDetailFields on Product {
  ...ProductFields
  description
  ingredientStatement
  supplierName
  frozen
  isAgeLimitedByAlcohol
  countryName { fi }
  allergens { allergenTypeText levelOfContainmentCode }
  nutrients { name kcal kj ri value }
  measurement { grossWeight grossWeightUnit netWeight netWeightUnit pceApproxWeight }
  packagingLabels
}";

const STORE_FIELDS: &str = "
fragment StoreFields on StoreInfo {
  id
  name
  brand
  street
  postalCode
  city
}";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Store {
    pub id: String,
    pub name: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShoppingList {
    pub id: String,
    pub name: String,
    pub modified_at: Option<String>,
    #[serde(default)]
    pub shared: bool,
    pub store_id: Option<String>,
    pub store: Option<Store>,
    #[serde(default)]
    pub items: Vec<Item>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: String,
    pub name: Option<String>,
    /// New items report `null` until they are first checked off.
    #[serde(default, deserialize_with = "null_as_false")]
    pub collected: bool,
    pub ean: Option<String>,
    pub quantity: Option<f64>,
    pub quantity_unit: Option<String>,
    pub sok_id: Option<String>,
    pub product: Option<ItemProduct>,
}

fn null_as_false<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    Ok(Option::<bool>::deserialize(deserializer)?.unwrap_or(false))
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemProduct {
    pub pricing: Option<Pricing>,
    pub location: Option<Location>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pricing {
    pub current_price: Option<f64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    pub aisle: Option<String>,
    pub label: Option<String>,
}

/// Fields to change on an item; unset fields are left as they are.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemChanges {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collected: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quantity: Option<f64>,
}

/// A product to add to a list as a product item.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProductItem {
    pub ean: String,
    pub name: String,
    pub quantity: f64,
    pub quantity_unit: String,
    /// The product id.
    pub sok_id: String,
    pub is_replaceable: bool,
}

impl ProductItem {
    /// Builds the item the way the app does from a product returned by the
    /// API: the unit is the product's basic quantity unit, else its price unit.
    pub fn from_product(product: &Value, quantity: f64) -> Result<Self> {
        let text = |field: &str| product[field].as_str().map(str::to_owned);
        let id = text("id").context("product has no id")?;
        Ok(Self {
            ean: text("ean").with_context(|| format!("product {id} has no EAN"))?,
            name: text("name").with_context(|| format!("product {id} has no name"))?,
            quantity,
            quantity_unit: text("basicQuantityUnit")
                .or_else(|| text("priceUnit"))
                .unwrap_or_else(|| "KPL".to_owned()),
            sok_id: id,
            is_replaceable: false,
        })
    }
}

#[derive(Debug, Deserialize)]
struct GraphqlResponse {
    data: Option<Value>,
    #[serde(default)]
    errors: Vec<GraphqlError>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlError {
    message: String,
    error_type: Option<String>,
}

impl GraphqlError {
    fn is_unauthorized(&self) -> bool {
        self.error_type.as_deref() == Some("UnauthorizedException")
    }
}

#[derive(Clone)]
pub struct Api {
    http: reqwest::Client,
    session: Arc<Session>,
}

impl Api {
    pub fn new(http: reqwest::Client, session: Arc<Session>) -> Self {
        Self { http, session }
    }

    /// Runs a query and returns its `data`. User queries are retried once
    /// with a refreshed token when the access token is rejected.
    async fn execute(&self, query: &str, variables: Value, access: Access) -> Result<Value> {
        let body = json!({ "query": query, "variables": variables });
        if access == Access::Public {
            return match self.send(Credential::ApiKey, &body).await? {
                Outcome::Data(data) => Ok(data),
                Outcome::Unauthorized => bail!("the API rejected the public API key"),
            };
        }
        let token = self.session.access_token().await?;
        match self.send(Credential::User(&token), &body).await? {
            Outcome::Data(data) => Ok(data),
            Outcome::Unauthorized => {
                let token = self.session.access_token_after_rejection(&token).await?;
                match self.send(Credential::User(&token), &body).await? {
                    Outcome::Data(data) => Ok(data),
                    Outcome::Unauthorized => {
                        bail!("the API rejected a freshly refreshed token; run `kassi login` again")
                    }
                }
            }
        }
    }

    async fn send(&self, credential: Credential<'_>, body: &Value) -> Result<Outcome> {
        let request = self.http.post(GRAPHQL_URL);
        let request = match credential {
            Credential::User(token) => request.header(reqwest::header::AUTHORIZATION, token),
            Credential::ApiKey => request.header("x-api-key", PUBLIC_API_KEY),
        };
        let response = request
            .json(body)
            .send()
            .await
            .context("calling the S-ostoslista API")?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        let parsed: Option<GraphqlResponse> = serde_json::from_str(&text).ok();
        if status == StatusCode::UNAUTHORIZED
            || parsed
                .as_ref()
                .is_some_and(|r| r.errors.iter().any(GraphqlError::is_unauthorized))
        {
            return Ok(Outcome::Unauthorized);
        }
        let Some(parsed) = parsed else {
            bail!(
                "API returned {status} with an unexpected body: {}",
                text.chars().take(300).collect::<String>()
            );
        };
        if !parsed.errors.is_empty() {
            let messages: Vec<_> = parsed.errors.iter().map(|e| e.message.as_str()).collect();
            bail!("API error: {}", messages.join("; "));
        }
        if !status.is_success() {
            bail!("API returned {status}");
        }
        parsed
            .data
            .map(Outcome::Data)
            .context("API response has no data")
    }

    /// Runs a query and decodes one top-level field, `None` when it is null.
    async fn optional_field<T: DeserializeOwned>(
        &self,
        query: &str,
        variables: Value,
        field: &str,
        access: Access,
    ) -> Result<Option<T>> {
        let mut data = self.execute(query, variables, access).await?;
        let value = data
            .get_mut(field)
            .map(Value::take)
            .with_context(|| format!("API response has no `{field}`"))?;
        if value.is_null() {
            return Ok(None);
        }
        serde_json::from_value(value)
            .map(Some)
            .with_context(|| format!("unexpected `{field}` shape"))
    }

    async fn field<T: DeserializeOwned>(
        &self,
        query: &str,
        variables: Value,
        field: &str,
    ) -> Result<T> {
        self.optional_field(query, variables, field, Access::User)
            .await?
            .context("not found")
    }

    async fn list_mutation(
        &self,
        mutation: &str,
        variables: Value,
        field: &str,
    ) -> Result<ShoppingList> {
        self.field(&format!("{mutation}{LIST_FIELDS}"), variables, field)
            .await
    }

    pub async fn user_name(&self) -> Result<Option<String>> {
        let user: Value = self
            .field("query { user { firstName } }", json!({}), "user")
            .await?;
        Ok(user["firstName"].as_str().map(str::to_owned))
    }

    pub async fn shopping_lists(&self) -> Result<Vec<ShoppingList>> {
        let query = format!("query {{ shoppingLists {{ ...ListFields }} }}{LIST_FIELDS}");
        self.field(&query, json!({}), "shoppingLists").await
    }

    /// Creates a list in planning mode, the mode the app uses for new lists.
    pub async fn create_list(
        &self,
        name: &str,
        store_id: Option<&str>,
        items: &[String],
    ) -> Result<ShoppingList> {
        let items: Vec<_> = items.iter().map(|name| json!({ "name": name })).collect();
        self.list_mutation(
            "mutation ($name: String!, $storeId: String, $items: [CreateShoppingListItemInput!]) {
  createShoppingList(name: $name, storeId: $storeId, items: $items, mode: PLAN) { ...ListFields }
}",
            json!({ "name": name, "storeId": store_id, "items": items }),
            "createShoppingList",
        )
        .await
    }

    pub async fn update_list(
        &self,
        id: &str,
        name: Option<&str>,
        store_id: Option<&str>,
    ) -> Result<ShoppingList> {
        self.list_mutation(
            "mutation ($id: ID!, $name: String, $storeId: String) {
  updateShoppingList(id: $id, name: $name, storeId: $storeId) { ...ListFields }
}",
            json!({ "id": id, "name": name, "storeId": store_id }),
            "updateShoppingList",
        )
        .await
    }

    pub async fn duplicate_list(
        &self,
        id: &str,
        name: &str,
        store_id: Option<&str>,
        include_collected: bool,
    ) -> Result<ShoppingList> {
        self.list_mutation(
            "mutation ($id: ID!, $name: String!, $storeId: String, $includeCollectedItems: Boolean) {
  duplicateShoppingList(id: $id, name: $name, storeId: $storeId, mode: PLAN,
    includeCollectedItems: $includeCollectedItems) { ...ListFields }
}",
            json!({
                "id": id,
                "name": name,
                "storeId": store_id,
                "includeCollectedItems": include_collected,
            }),
            "duplicateShoppingList",
        )
        .await
    }

    pub async fn delete_list(&self, id: &str) -> Result<()> {
        let _: Value = self
            .field(
                "mutation ($id: ID!) { deleteShoppingList(id: $id) { id } }",
                json!({ "id": id }),
                "deleteShoppingList",
            )
            .await?;
        Ok(())
    }

    /// Returns a link that lets whoever opens it read, edit, and reshare the
    /// list, valid for a year, as the app's share button does.
    pub async fn share_link(&self, id: &str) -> Result<String> {
        let link: Value = self
            .field(
                "mutation ($id: ID!) {
  generateShoppingListShareLink(id: $id, permissions: [READ, WRITE, SHARE], validForHours: 8760) { id }
}",
                json!({ "id": id }),
                "generateShoppingListShareLink",
            )
            .await?;
        let link_id = link["id"].as_str().context("share link has no id")?;
        Ok(format!("{SHARE_URL}{link_id}"))
    }

    pub async fn claim_shared_list(&self, link_id: &str) -> Result<ShoppingList> {
        self.list_mutation(
            "mutation ($linkId: ID!) { claimSharedShoppingList(linkId: $linkId) { ...ListFields } }",
            json!({ "linkId": link_id }),
            "claimSharedShoppingList",
        )
        .await
    }

    pub async fn add_items(
        &self,
        list_id: &str,
        names: &[String],
        products: &[ProductItem],
    ) -> Result<ShoppingList> {
        let items: Vec<_> = names.iter().map(|name| json!({ "name": name })).collect();
        self.list_mutation(
            "mutation ($shoppingListId: ID!, $items: [CreateShoppingListItemInput!],
  $productResolvedItems: [CreateProductResolvedShoppingListItemInput!]) {
  batchInsertShoppingListItem(shoppingListId: $shoppingListId, items: $items,
    productResolvedItems: $productResolvedItems) { ...ListFields }
}",
            json!({ "shoppingListId": list_id, "items": items, "productResolvedItems": products }),
            "batchInsertShoppingListItem",
        )
        .await
    }

    pub async fn update_items(
        &self,
        list_id: &str,
        changes: &[(String, ItemChanges)],
    ) -> Result<ShoppingList> {
        let (ids, items): (Vec<_>, Vec<_>) = changes.iter().cloned().unzip();
        self.list_mutation(
            "mutation ($shoppingListId: ID!, $ids: [ID!]!, $items: [UpdateShoppingListItemInput!]!) {
  batchUpdateShoppingListItem(shoppingListId: $shoppingListId, ids: $ids, items: $items) { ...ListFields }
}",
            json!({ "shoppingListId": list_id, "ids": ids, "items": items }),
            "batchUpdateShoppingListItem",
        )
        .await
    }

    pub async fn remove_items(&self, list_id: &str, ids: &[String]) -> Result<ShoppingList> {
        self.list_mutation(
            "mutation ($shoppingListId: ID!, $ids: [ID!]!) {
  batchDeleteShoppingListItem(shoppingListId: $shoppingListId, ids: $ids) { ...ListFields }
}",
            json!({ "shoppingListId": list_id, "ids": ids }),
            "batchDeleteShoppingListItem",
        )
        .await
    }

    /// Links an item to a product, turning it into a product item.
    pub async fn resolve_item(
        &self,
        list_id: &str,
        item_id: &str,
        product_id: &str,
    ) -> Result<ShoppingList> {
        self.list_mutation(
            "mutation ($shoppingListId: ID!, $id: ID!, $sokId: String!) {
  resolveShoppingListItemProduct(shoppingListId: $shoppingListId, id: $id, sokId: $sokId) { ...ListFields }
}",
            json!({ "shoppingListId": list_id, "id": item_id, "sokId": product_id }),
            "resolveShoppingListItemProduct",
        )
        .await
    }

    /// Turns a product item back into a free-text item.
    pub async fn unresolve_item(&self, list_id: &str, item_id: &str) -> Result<ShoppingList> {
        self.list_mutation(
            "mutation ($shoppingListId: ID!, $id: ID!) {
  unresolveShoppingListItemProduct(shoppingListId: $shoppingListId, id: $id) { ...ListFields }
}",
            json!({ "shoppingListId": list_id, "id": item_id }),
            "unresolveShoppingListItemProduct",
        )
        .await
    }

    /// Searches one store's assortment, or every store's when `store_id` is
    /// `None`. Global search fails when `storeId` is passed at all, even as
    /// null, so the argument is left out of that query.
    pub async fn search_products(
        &self,
        query: &str,
        store_id: Option<&str>,
        from: u32,
        limit: u32,
    ) -> Result<Value> {
        let scope = match store_id {
            Some(_) => "storeId: $storeId",
            None => "globalAssortment: true",
        };
        let declare_store = if store_id.is_some() {
            ", $storeId: ID"
        } else {
            ""
        };
        self.field(
            &format!(
                "query ($query: String, $from: Int, $pageSize: Int{declare_store}) {{
  productSearch(query: $query, {scope}, from: $from, pageSize: $pageSize) {{
    maybeHasMore items {{ ...ProductFields }}
  }}
}}{PRODUCT_FIELDS}"
            ),
            json!({ "query": query, "storeId": store_id, "from": from, "pageSize": limit }),
            "productSearch",
        )
        .await
    }

    /// Looks up a product by id. Only the public API key may read products
    /// directly.
    pub async fn product(&self, id: &str, store_id: Option<&str>) -> Result<Value> {
        self.product_lookup("product(id: $key", "ID!", id, store_id, "product")
            .await
            .with_context(|| format!("product {id}"))
    }

    pub async fn product_by_ean(&self, ean: &str, store_id: Option<&str>) -> Result<Value> {
        self.product_lookup(
            "productByEan(ean: $key",
            "ID!",
            ean,
            store_id,
            "productByEan",
        )
        .await
        .with_context(|| format!("product with EAN {ean}"))
    }

    async fn product_lookup(
        &self,
        call: &str,
        key_type: &str,
        key: &str,
        store_id: Option<&str>,
        field: &str,
    ) -> Result<Value> {
        let (declare_store, scope) = match store_id {
            Some(_) => (", $storeId: ID", "storeId: $storeId"),
            None => ("", "globalAssortment: true"),
        };
        self.optional_field(
            &format!(
                "query ($key: {key_type}{declare_store}) {{
  {call}, {scope}, ignoreAvailability: true) {{ ...ProductDetailFields }}
}}{PRODUCT_DETAIL_FIELDS}{PRODUCT_FIELDS}"
            ),
            json!({ "key": key, "storeId": store_id }),
            field,
            Access::Public,
        )
        .await?
        .context("not found")
    }

    /// Product suggestions for a list, grouped by category, with
    /// `historySource`: `BOA` when they come from the user's card purchases,
    /// `S_OSTOSLISTA` when from items collected in the app. On an empty list
    /// with a store they amount to the user's purchase history. Lists
    /// without a store get none.
    pub async fn recommendations(
        &self,
        list_id: &str,
        per_group: u32,
        total: u32,
    ) -> Result<Value> {
        let list: Value = self
            .field(
                &format!(
                    "query ($id: ID!, $perGroup: Int, $total: Int) {{
  shoppingList(id: $id) {{
    recommendations(maxGroupSize: $perGroup, numOfRecommendations: $total) {{
      historySource
      groups {{ groupName products {{ ...ProductFields }} }}
    }}
  }}
}}{PRODUCT_FIELDS}"
                ),
                json!({ "id": list_id, "perGroup": per_group, "total": total }),
                "shoppingList",
            )
            .await?;
        Ok(list["recommendations"].clone())
    }

    pub async fn search_stores(&self, query: &str, brands: &[String], limit: u32) -> Result<Value> {
        let brands = (!brands.is_empty()).then_some(brands);
        self.field(
            &format!(
                "query ($query: String, $brands: [StoreBrand!], $pageSize: Int) {{
  stores(query: $query, brands: $brands, pageSize: $pageSize) {{
    totalCount stores {{ ...StoreFields }}
  }}
}}{STORE_FIELDS}"
            ),
            json!({ "query": query, "brands": brands, "pageSize": limit }),
            "stores",
        )
        .await
    }

    pub async fn store(&self, id: &str) -> Result<Value> {
        self.field(
            &format!(
                "query ($id: ID!) {{
  store(id: $id) {{
    ...StoreFields
    contactInfo {{ email phoneNumber {{ number }} }}
    services {{ name {{ default }} }}
    weeklyOpeningHours {{
      weekNumber
      openingTimes {{ date day mode exceptional ranges {{ open close }} message {{ fi en }} }}
    }}
  }}
}}{STORE_FIELDS}"
            ),
            json!({ "id": id }),
            "store",
        )
        .await
        .with_context(|| format!("store {id}"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Access {
    User,
    Public,
}

enum Credential<'a> {
    User(&'a str),
    ApiKey,
}

enum Outcome {
    Data(Value),
    Unauthorized,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_both_item_kinds() {
        let list: ShoppingList = serde_json::from_value(json!({
            "id": "l1", "name": "Ruoka",
            "modifiedAt": "2026-11-23T10:00:00.000Z",
            "store": { "id": "s1", "name": "Prisma" },
            "items": [
                { "id": "a", "name": "maito", "collected": null },
                { "id": "b", "name": "Valio rasvaton maito 1l", "collected": true,
                  "ean": "6408430000258", "quantity": 2.0, "quantityUnit": "KPL",
                  "sokId": "100", "product": {
                      "pricing": { "currentPrice": 1.19 },
                      "location": { "aisle": "1006", "label": "MAITO-OSASTO" } } }
            ]
        }))
        .unwrap();
        assert!(!list.items[0].collected);
        assert!(list.items[1].collected);
        assert_eq!(list.items[1].quantity, Some(2.0));
        let product = list.items[1].product.as_ref().unwrap();
        assert_eq!(product.pricing.as_ref().unwrap().current_price, Some(1.19));
        assert_eq!(
            product.location.as_ref().unwrap().aisle.as_deref(),
            Some("1006")
        );
    }

    #[test]
    fn product_item_uses_basic_unit_then_price_unit() {
        let product = json!({ "id": "100", "ean": "641", "name": "Maito",
            "basicQuantityUnit": null, "priceUnit": "KG" });
        let item = ProductItem::from_product(&product, 2.0).unwrap();
        assert_eq!(item.quantity_unit, "KG");
        assert_eq!(item.sok_id, "100");
        assert_eq!(
            serde_json::to_value(&item).unwrap(),
            json!({ "ean": "641", "name": "Maito", "quantity": 2.0, "quantityUnit": "KG",
                "sokId": "100", "isReplaceable": false })
        );
        let with_basic = json!({ "id": "1", "ean": "2", "name": "x",
            "basicQuantityUnit": "KPL", "priceUnit": "KG" });
        assert_eq!(
            ProductItem::from_product(&with_basic, 1.0)
                .unwrap()
                .quantity_unit,
            "KPL"
        );
        assert!(ProductItem::from_product(&json!({ "id": "1" }), 1.0).is_err());
    }

    #[test]
    fn item_changes_send_only_the_fields_set() {
        let changes = ItemChanges {
            collected: Some(true),
            ..ItemChanges::default()
        };
        assert_eq!(
            serde_json::to_value(changes).unwrap(),
            json!({ "collected": true })
        );
    }

    #[test]
    fn recognises_appsync_unauthorized_errors() {
        let response: GraphqlResponse = serde_json::from_value(json!({
            "data": null,
            "errors": [{ "errorType": "UnauthorizedException", "message": "Unauthorized" }]
        }))
        .unwrap();
        assert!(response.errors[0].is_unauthorized());
    }
}
