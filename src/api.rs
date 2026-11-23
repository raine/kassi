//! The S-ostoslista GraphQL API (AWS AppSync).
//!
//! User operations authenticate with the bare access token in `Authorization`.
//! Sending the public API key alongside it makes AppSync pick API-key auth,
//! which has no access to the user's lists, so the key is never sent here.

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::auth::Session;

pub const GRAPHQL_URL: &str = "https://api.s-ostoslista.fi/graphql";

const LIST_FIELDS: &str = "
fragment ListFields on ShoppingList {
  id
  name
  modifiedAt
  store { id name }
  items {
    id
    name
    collected
    ... on ProductResolvedShoppingListItem {
      ean
      quantity
      quantityUnit
      product { pricing { currentPrice } }
    }
  }
}";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Store {
    pub id: String,
    pub name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreInfo {
    pub id: String,
    pub name: Option<String>,
    pub brand: Option<String>,
    pub street: Option<String>,
    pub city: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShoppingList {
    pub id: String,
    pub name: String,
    pub modified_at: Option<String>,
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
    pub product: Option<Product>,
}

fn null_as_false<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    Ok(Option::<bool>::deserialize(deserializer)?.unwrap_or(false))
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Product {
    pub pricing: Option<Pricing>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pricing {
    pub current_price: Option<f64>,
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

    /// Runs a query and returns its `data`, retrying once with a refreshed
    /// token when the access token is rejected.
    async fn execute(&self, query: &str, variables: Value) -> Result<Value> {
        let body = json!({ "query": query, "variables": variables });
        let token = self.session.access_token().await?;
        match self.send(&token, &body).await? {
            Outcome::Data(data) => Ok(data),
            Outcome::Unauthorized => {
                let token = self.session.access_token_after_rejection(&token).await?;
                match self.send(&token, &body).await? {
                    Outcome::Data(data) => Ok(data),
                    Outcome::Unauthorized => {
                        bail!("the API rejected a freshly refreshed token; run `kassi login` again")
                    }
                }
            }
        }
    }

    async fn send(&self, token: &str, body: &Value) -> Result<Outcome> {
        let response = self
            .http
            .post(GRAPHQL_URL)
            .header(reqwest::header::AUTHORIZATION, token)
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

    async fn field<T: DeserializeOwned>(
        &self,
        query: &str,
        variables: Value,
        field: &str,
    ) -> Result<T> {
        let mut data = self.execute(query, variables).await?;
        let value = data
            .get_mut(field)
            .map(Value::take)
            .with_context(|| format!("API response has no `{field}`"))?;
        if value.is_null() {
            bail!("not found");
        }
        serde_json::from_value(value).with_context(|| format!("unexpected `{field}` shape"))
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

    pub async fn shopping_list(&self, id: &str) -> Result<ShoppingList> {
        let query = format!(
            "query ($id: ID!) {{ shoppingList(id: $id) {{ ...ListFields }} }}{LIST_FIELDS}"
        );
        self.field(&query, json!({ "id": id }), "shoppingList")
            .await
            .with_context(|| format!("shopping list {id}"))
    }

    pub async fn search_stores(&self, query: &str, limit: u32) -> Result<Vec<StoreInfo>> {
        #[derive(Deserialize)]
        struct Page {
            stores: Vec<StoreInfo>,
        }
        let page: Page = self
            .field(
                "query ($query: String, $pageSize: Int) {
  stores(query: $query, pageSize: $pageSize) { stores { id name brand street city } }
}",
                json!({ "query": query, "pageSize": limit }),
                "stores",
            )
            .await?;
        Ok(page.stores)
    }

    /// Creates a list in planning mode, the mode the app uses for new lists.
    pub async fn create_list(&self, name: &str, store_id: &str) -> Result<ShoppingList> {
        #[derive(Deserialize)]
        struct Created {
            id: String,
        }
        let created: Created = self
            .field(
                "mutation ($name: String!, $storeId: String!) {
  createShoppingList(name: $name, storeId: $storeId, mode: PLAN) { id }
}",
                json!({ "name": name, "storeId": store_id }),
                "createShoppingList",
            )
            .await?;
        self.shopping_list(&created.id).await
    }

    pub async fn add_items(&self, list_id: &str, names: &[String]) -> Result<ShoppingList> {
        let items: Vec<_> = names.iter().map(|name| json!({ "name": name })).collect();
        let query = format!(
            "mutation ($shoppingListId: ID!, $items: [CreateShoppingListItemInput!]) {{
  batchInsertShoppingListItem(shoppingListId: $shoppingListId, items: $items) {{ ...ListFields }}
}}{LIST_FIELDS}"
        );
        let variables = json!({ "shoppingListId": list_id, "items": items });
        self.field(&query, variables, "batchInsertShoppingListItem")
            .await
    }

    pub async fn update_items(
        &self,
        list_id: &str,
        changes: &[(String, ItemChanges)],
    ) -> Result<ShoppingList> {
        let (ids, items): (Vec<_>, Vec<_>) = changes.iter().cloned().unzip();
        let query = format!(
            "mutation ($shoppingListId: ID!, $ids: [ID!]!, $items: [UpdateShoppingListItemInput!]!) {{
  batchUpdateShoppingListItem(shoppingListId: $shoppingListId, ids: $ids, items: $items) {{ ...ListFields }}
}}{LIST_FIELDS}"
        );
        let variables = json!({ "shoppingListId": list_id, "ids": ids, "items": items });
        self.field(&query, variables, "batchUpdateShoppingListItem")
            .await
    }

    pub async fn remove_items(&self, list_id: &str, ids: &[String]) -> Result<ShoppingList> {
        let query = format!(
            "mutation ($shoppingListId: ID!, $ids: [ID!]!) {{
  batchDeleteShoppingListItem(shoppingListId: $shoppingListId, ids: $ids) {{ ...ListFields }}
}}{LIST_FIELDS}"
        );
        let variables = json!({ "shoppingListId": list_id, "ids": ids });
        self.field(&query, variables, "batchDeleteShoppingListItem")
            .await
    }
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
                  "ean": "6408430000258", "quantity": 2.0, "quantityUnit": "pcs",
                  "product": { "pricing": { "currentPrice": 1.19 } } }
            ]
        }))
        .unwrap();
        assert_eq!(list.items.len(), 2);
        assert!(!list.items[0].collected);
        assert!(list.items[1].collected);
        assert_eq!(list.items[1].quantity, Some(2.0));
        assert_eq!(
            list.items[1]
                .product
                .as_ref()
                .unwrap()
                .pricing
                .as_ref()
                .unwrap()
                .current_price,
            Some(1.19)
        );
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
