//! Tool implementations, independent of the MCP transport.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::sync::Mutex;

use crate::api::{Api, Item, ItemChanges, ProductItem, SHARE_URL, ShoppingList};
use crate::storemap::{self, StoreMap, Wanted};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListArgs {
    /// List id or name. May be omitted when the user has only one list.
    pub list: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetListArgs {
    /// List id or name. May be omitted when the user has only one list.
    pub list: Option<String>,
    /// Include items that are already collected. Defaults to true.
    pub include_collected: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateListArgs {
    /// Name for the new list.
    pub name: String,
    /// Store id from search_stores. Product prices, availability, and aisles
    /// follow this store. Optional.
    pub store_id: Option<String>,
    /// Free-text items to start the list with.
    #[serde(default)]
    pub items: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateListArgs {
    /// List id or name. May be omitted when the user has only one list.
    pub list: Option<String>,
    /// New name.
    pub name: Option<String>,
    /// New store id from search_stores.
    pub store_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DuplicateListArgs {
    /// List id or name. May be omitted when the user has only one list.
    pub list: Option<String>,
    /// Name for the copy.
    pub name: String,
    /// Store for the copy. Defaults to the original's store.
    pub store_id: Option<String>,
    /// Copy items that are already collected too. Defaults to false.
    pub include_collected: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteListArgs {
    /// Id of the list to delete. Must be the id, not the name.
    pub list_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JoinListArgs {
    /// Share link (https://s-ostoslista.fi/liity/...) or its id.
    pub link: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddItemsArgs {
    /// List id or name. May be omitted when the user has only one list.
    pub list: Option<String>,
    /// Item names to add, one item per entry, e.g. ["maito", "2 kg perunoita"].
    pub items: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddProductsArgs {
    /// List id or name. May be omitted when the user has only one list.
    pub list: Option<String>,
    /// Products to add.
    pub products: Vec<ProductToAdd>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProductToAdd {
    /// Product id from search_products, get_recommendations, or
    /// purchase_history.
    pub product_id: String,
    /// How many, in the product's unit (pieces, or kg for weighed products).
    /// Defaults to 1.
    pub quantity: Option<f64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateItemsArgs {
    /// List id or name. May be omitted when the user has only one list.
    pub list: Option<String>,
    /// Changes to apply, one entry per item.
    pub updates: Vec<ItemUpdate>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ItemUpdate {
    /// Item id from get_shopping_list.
    pub item_id: String,
    /// true checks the item off as bought; false puts it back on the list.
    pub collected: Option<bool>,
    /// New item name.
    pub name: Option<String>,
    /// New quantity. Applies to product items.
    pub quantity: Option<f64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetItemProductArgs {
    /// List id or name. May be omitted when the user has only one list.
    pub list: Option<String>,
    /// Item id from get_shopping_list.
    pub item_id: String,
    /// Product id from search_products. Omit to turn a product item back
    /// into a free-text item.
    pub product_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveItemsArgs {
    /// List id or name. May be omitted when the user has only one list.
    pub list: Option<String>,
    /// Ids of the items to delete, from get_shopping_list.
    pub item_ids: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchProductsArgs {
    /// What to look for, in Finnish for best results, e.g. "kevytmaito" or
    /// "kahvi tumma paahto".
    pub query: String,
    /// Store id for that store's prices, availability, and aisles. Omit to
    /// search the whole S-ryhmä assortment.
    pub store_id: Option<String>,
    /// Results to return, 1-50. Defaults to 10.
    pub limit: Option<u32>,
    /// Results to skip, for paging. Defaults to 0.
    pub offset: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetProductArgs {
    /// Product id. Give this or `ean`.
    pub product_id: Option<String>,
    /// Barcode (EAN). Give this or `product_id`.
    pub ean: Option<String>,
    /// Store id for that store's price, availability, and aisle.
    pub store_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecommendationsArgs {
    /// List id or name. May be omitted when the user has only one list.
    pub list: Option<String>,
    /// Suggestions per group, 1-20. Defaults to 8.
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PurchaseHistoryArgs {
    /// Store id from search_stores or a list. Products and prices are those
    /// of this store.
    pub store_id: String,
    /// Most products per category, 1-50. Defaults to 20.
    pub per_category: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StoreArgs {
    /// Store id from search_stores or a list.
    pub store_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchStoresArgs {
    /// Store name part, district, or city, e.g. "kalasatama" or "tampere".
    /// Single words match best.
    pub query: String,
    /// Only these chains: PRISMA, S_MARKET, ALEPA, SALE, ABC, HERKKU,
    /// SOKOS_HERKKU, MESTARIN_HERKKU.
    #[serde(default)]
    pub brands: Vec<String>,
    /// Stores to return, 1-50. Defaults to 10.
    pub limit: Option<u32>,
}

/// The API's upper limit for `numOfRecommendations`.
const MAX_RECOMMENDATIONS: u32 = 500;

/// Products per category fetched for purchase history; purchase_history
/// trims this to what the caller asked for.
const HISTORY_PER_CATEGORY: u32 = 50;

/// How long a store's purchase history is reused before fetching it again.
const HISTORY_TTL: Duration = Duration::from_secs(60 * 60);

/// Search results fetched on the first page so that previously bought
/// products ranked below the requested limit can still be moved up.
const SEARCH_WINDOW: u32 = 50;

/// Name of the empty list created and deleted again to read purchase history.
const TEMPORARY_LIST_NAME: &str = "kassi: ostohistoria (väliaikainen)";

const BRANDS: &[&str] = &[
    "ABC",
    "ALEPA",
    "HERKKU",
    "MESTARIN_HERKKU",
    "PRISMA",
    "SALE",
    "SOKOS_HERKKU",
    "S_MARKET",
];

pub struct Tools {
    api: Api,
    /// Purchase history by store id. The mutex also keeps two fetches from
    /// running at once, since each one deletes leftover temporary lists.
    history: Mutex<HashMap<String, (Instant, Arc<History>)>>,
    /// Indoor maps by store id. Floor plans rarely change, so they are kept
    /// for as long as the server runs.
    maps: Mutex<HashMap<String, Arc<StoreMap>>>,
}

/// A walking route through a store for a shopping list: a summary for the
/// assistant and a drawing for the user.
pub struct RouteMap {
    pub summary: Value,
    pub svg: String,
}

impl RouteMap {
    pub fn png(&self) -> Result<Vec<u8>> {
        storemap::png(&self.svg)
    }
}

/// A store's purchase history as the API returns it, with the ids of its
/// products for quick lookups.
struct History {
    recommendations: Value,
    product_ids: HashSet<String>,
}

impl History {
    fn new(recommendations: Value) -> Self {
        let product_ids = recommendations["groups"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|group| group["products"].as_array().into_iter().flatten())
            .filter_map(|product| product["id"].as_str().map(str::to_owned))
            .collect();
        Self {
            recommendations,
            product_ids,
        }
    }

    fn contains(&self, product: &Value) -> bool {
        product["id"]
            .as_str()
            .is_some_and(|id| self.product_ids.contains(id))
    }
}

impl Tools {
    pub fn new(api: Api) -> Self {
        Self {
            api,
            history: Mutex::new(HashMap::new()),
            maps: Mutex::new(HashMap::new()),
        }
    }

    pub async fn list_shopping_lists(&self) -> Result<Value> {
        let lists = self.api.shopping_lists().await?;
        Ok(json!({ "lists": lists.iter().map(list_summary).collect::<Vec<_>>() }))
    }

    pub async fn get_shopping_list(&self, args: GetListArgs) -> Result<Value> {
        let list = self.resolve(args.list.as_deref()).await?;
        Ok(list_detail(&list, args.include_collected.unwrap_or(true)))
    }

    pub async fn create_shopping_list(&self, args: CreateListArgs) -> Result<Value> {
        let name = required("name", &args.name)?;
        let items = clean_names(&args.items);
        let store_id = optional(args.store_id.as_deref());
        let list = self.api.create_list(name, store_id, &items).await?;
        Ok(json!({ "list": list_detail(&list, true) }))
    }

    pub async fn update_shopping_list(&self, args: UpdateListArgs) -> Result<Value> {
        let name = optional(args.name.as_deref());
        let store_id = optional(args.store_id.as_deref());
        if name.is_none() && store_id.is_none() {
            bail!("give a new name, a new store_id, or both");
        }
        let list = self.resolve(args.list.as_deref()).await?;
        let list = self.api.update_list(&list.id, name, store_id).await?;
        Ok(json!({ "list": list_summary(&list) }))
    }

    pub async fn duplicate_shopping_list(&self, args: DuplicateListArgs) -> Result<Value> {
        let name = required("name", &args.name)?;
        let list = self.resolve(args.list.as_deref()).await?;
        let store_id = optional(args.store_id.as_deref()).or(list.store_id.as_deref());
        let copy = self
            .api
            .duplicate_list(
                &list.id,
                name,
                store_id,
                args.include_collected.unwrap_or(false),
            )
            .await?;
        Ok(json!({ "list": list_summary(&copy) }))
    }

    pub async fn delete_shopping_list(&self, args: DeleteListArgs) -> Result<Value> {
        let lists = self.api.shopping_lists().await?;
        let Some(list) = lists.iter().find(|list| list.id == args.list_id.trim()) else {
            bail!(
                "no list has id {:?}; delete_shopping_list needs the exact id from list_shopping_lists",
                args.list_id
            );
        };
        self.api.delete_list(&list.id).await?;
        Ok(json!({ "deleted": list_summary(list) }))
    }

    pub async fn share_shopping_list(&self, args: ListArgs) -> Result<Value> {
        let list = self.resolve(args.list.as_deref()).await?;
        let url = self.api.share_link(&list.id).await?;
        Ok(json!({
            "list": list_summary(&list),
            "url": url,
            "note": "Anyone who opens the link in the S-ostoslista app can view, edit, and reshare the list for a year.",
        }))
    }

    pub async fn join_shared_list(&self, args: JoinListArgs) -> Result<Value> {
        let link_id = share_link_id(&args.link)?;
        let list = self.api.claim_shared_list(&link_id).await?;
        Ok(json!({ "list": list_summary(&list) }))
    }

    pub async fn add_items(&self, args: AddItemsArgs) -> Result<Value> {
        let names = clean_names(&args.items);
        if names.is_empty() {
            bail!("items must contain at least one non-empty name");
        }
        let before = self.resolve(args.list.as_deref()).await?;
        let after = self.api.add_items(&before.id, &names, &[]).await?;
        Ok(json!({ "list": list_summary(&after), "added": new_items(&before, &after) }))
    }

    pub async fn add_products(&self, args: AddProductsArgs) -> Result<Value> {
        if args.products.is_empty() {
            bail!("products must contain at least one product");
        }
        let before = self.resolve(args.list.as_deref()).await?;
        let ids: Vec<String> = args
            .products
            .iter()
            .map(|p| p.product_id.trim().to_owned())
            .collect();
        let mut items = Vec::with_capacity(ids.len());
        for (id, wanted) in ids.iter().zip(&args.products) {
            let quantity = wanted.quantity.unwrap_or(1.0);
            if !(quantity > 0.0 && quantity.is_finite()) {
                bail!("quantity for {id} must be positive");
            }
            let product = self
                .api
                .product(id, before.store_id.as_deref())
                .await
                .context("find product ids with search_products")?;
            items.push(ProductItem::from_product(&product, quantity)?);
        }
        let after = self.api.add_items(&before.id, &[], &items).await?;
        Ok(json!({ "list": list_summary(&after), "added": new_items(&before, &after) }))
    }

    pub async fn update_items(&self, args: UpdateItemsArgs) -> Result<Value> {
        if args.updates.is_empty() {
            bail!("updates must contain at least one item");
        }
        let list = self.resolve(args.list.as_deref()).await?;
        let mut changes = Vec::with_capacity(args.updates.len());
        for update in args.updates {
            if update.collected.is_none() && update.name.is_none() && update.quantity.is_none() {
                bail!("update for {} changes nothing", update.item_id);
            }
            require_item(&list, &update.item_id)?;
            let change = ItemChanges {
                name: update.name,
                collected: update.collected,
                quantity: update.quantity,
            };
            changes.push((update.item_id, change));
        }
        let ids: HashSet<_> = changes.iter().map(|(id, _)| id.clone()).collect();
        let after = self.api.update_items(&list.id, &changes).await?;
        let updated: Vec<_> = after
            .items
            .iter()
            .filter(|item| ids.contains(&item.id))
            .map(item_view)
            .collect();
        Ok(json!({ "list": list_summary(&after), "updated": updated }))
    }

    pub async fn set_item_product(&self, args: SetItemProductArgs) -> Result<Value> {
        let list = self.resolve(args.list.as_deref()).await?;
        require_item(&list, &args.item_id)?;
        let after = match optional(args.product_id.as_deref()) {
            Some(product_id) => {
                self.api
                    .resolve_item(&list.id, &args.item_id, product_id)
                    .await?
            }
            None => self.api.unresolve_item(&list.id, &args.item_id).await?,
        };
        // Linking may replace the item, so report the whole list.
        Ok(list_detail(&after, true))
    }

    pub async fn remove_items(&self, args: RemoveItemsArgs) -> Result<Value> {
        if args.item_ids.is_empty() {
            bail!("item_ids must contain at least one id");
        }
        let list = self.resolve(args.list.as_deref()).await?;
        let removed: Vec<_> = args
            .item_ids
            .iter()
            .map(|id| require_item(&list, id).map(item_view))
            .collect::<Result<_>>()?;
        let after = self.api.remove_items(&list.id, &args.item_ids).await?;
        Ok(json!({ "list": list_summary(&after), "removed": removed }))
    }

    pub async fn clear_collected(&self, args: ListArgs) -> Result<Value> {
        let list = self.resolve(args.list.as_deref()).await?;
        let (ids, removed): (Vec<String>, Vec<Value>) = list
            .items
            .iter()
            .filter(|item| item.collected)
            .map(|item| (item.id.clone(), item_view(item)))
            .unzip();
        if ids.is_empty() {
            return Ok(json!({ "list": list_summary(&list), "removed": [] }));
        }
        let after = self.api.remove_items(&list.id, &ids).await?;
        Ok(json!({ "list": list_summary(&after), "removed": removed }))
    }

    pub async fn search_products(&self, args: SearchProductsArgs) -> Result<Value> {
        let query = required("query", &args.query)?;
        let limit = args.limit.unwrap_or(10).clamp(1, 50);
        let offset = args.offset.unwrap_or(0);
        let store_id = optional(args.store_id.as_deref());
        let history = match store_id {
            Some(store_id) => match self.store_history(store_id).await {
                Ok(history) => Some(history),
                Err(error) => {
                    tracing::warn!("searching without purchase history: {error:#}");
                    None
                }
            },
            None => None,
        };
        // On the first page, look further than the limit so that products
        // the user has bought can be moved to the top.
        let window = if history.is_some() && offset == 0 {
            limit.max(SEARCH_WINDOW)
        } else {
            limit
        };
        let page = self
            .api
            .search_products(query, store_id, offset, window)
            .await?;
        let items = page["items"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        let more = page["maybeHasMore"].as_bool().unwrap_or(false) || items.len() > limit as usize;
        let products: Vec<_> = bought_first(items, history.as_deref())
            .into_iter()
            .take(limit as usize)
            .map(|(product, bought)| {
                let mut view = scoped(product_view(product), store_id);
                if bought {
                    view["bought_before"] = json!(true);
                }
                view
            })
            .collect();
        Ok(prune(json!({
            "store_id": store_id,
            "products": products,
            "more": more,
        })))
    }

    pub async fn get_product(&self, args: GetProductArgs) -> Result<Value> {
        let store_id = optional(args.store_id.as_deref());
        let product = match (
            optional(args.product_id.as_deref()),
            optional(args.ean.as_deref()),
        ) {
            (Some(id), None) => self.api.product(id, store_id).await?,
            (None, Some(ean)) => self.api.product_by_ean(ean, store_id).await?,
            _ => bail!("give exactly one of product_id or ean"),
        };
        Ok(scoped(product_detail_view(&product), store_id))
    }

    pub async fn get_recommendations(&self, args: RecommendationsArgs) -> Result<Value> {
        let list = self.resolve(args.list.as_deref()).await?;
        if list.store_id.is_none() {
            bail!("the list has no store; set one with update_shopping_list to get suggestions");
        }
        let limit = args.limit.unwrap_or(8).clamp(1, 20);
        let recommendations = self
            .api
            .recommendations(&list.id, limit, limit * 20)
            .await?;
        Ok(json!({
            "list": list_summary(&list),
            "based_on": history_source(&recommendations),
            "groups": groups_view(&recommendations, product_view),
        }))
    }

    pub async fn purchase_history(&self, args: PurchaseHistoryArgs) -> Result<Value> {
        let store_id = required("store_id", &args.store_id)?;
        let per_category = args
            .per_category
            .unwrap_or(20)
            .clamp(1, HISTORY_PER_CATEGORY);
        let history = self.store_history(store_id).await?;
        let source = history_source(&history.recommendations);
        let mut categories = groups_view(&history.recommendations, history_product_view);
        for category in &mut categories {
            if let Some(products) = category["products"].as_array_mut() {
                products.truncate(per_category as usize);
            }
        }
        let mut result = json!({
            "store_id": store_id,
            "based_on": source,
            "categories": categories,
        });
        if source != "card_purchases" {
            result["note"] = json!(
                "No S-Etukortti purchase data is available, so these come from items checked off in the app."
            );
        }
        Ok(result)
    }

    /// The user's purchase history as products at a store, reused for
    /// `HISTORY_TTL`.
    async fn store_history(&self, store_id: &str) -> Result<Arc<History>> {
        let mut cache = self.history.lock().await;
        if let Some((fetched, history)) = cache.get(store_id)
            && fetched.elapsed() < HISTORY_TTL
        {
            return Ok(history.clone());
        }
        let history = Arc::new(History::new(self.fetch_history(store_id).await?));
        cache.insert(store_id.to_owned(), (Instant::now(), history.clone()));
        Ok(history)
    }

    /// The API exposes card purchase history only as suggestions for a list,
    /// and only an empty list gets the plain history, so this reads it from
    /// an empty temporary list for the store.
    async fn fetch_history(&self, store_id: &str) -> Result<Value> {
        self.delete_leftover_temporary_lists().await?;
        let list = self
            .api
            .create_list(TEMPORARY_LIST_NAME, Some(store_id), &[])
            .await?;
        let history = self
            .api
            .recommendations(&list.id, HISTORY_PER_CATEGORY, MAX_RECOMMENDATIONS)
            .await;
        if let Err(error) = self.api.delete_list(&list.id).await {
            tracing::warn!("could not delete temporary list {}: {error:#}", list.id);
        }
        history
    }

    /// Removes temporary lists that an interrupted history fetch left.
    async fn delete_leftover_temporary_lists(&self) -> Result<()> {
        for list in self.api.shopping_lists().await? {
            if list.name == TEMPORARY_LIST_NAME && list.items.is_empty() {
                self.api.delete_list(&list.id).await?;
            }
        }
        Ok(())
    }

    /// Orders the list's uncollected items into a walk from the entrance to
    /// the checkout of the list's store, and draws it on the store map.
    pub async fn route_map(&self, args: ListArgs) -> Result<RouteMap> {
        let list = self.resolve(args.list.as_deref()).await?;
        let Some(store_id) = list.store_id.as_deref() else {
            bail!("the list has no store; set one with update_shopping_list to plan a route");
        };
        let map = self.store_map(store_id).await?;
        let open: Vec<&Item> = list.items.iter().filter(|item| !item.collected).collect();
        let wanted: Vec<Wanted> = open
            .iter()
            .map(|item| Wanted {
                names: vec![item.name.clone().unwrap_or_default()],
                sections: item
                    .product
                    .as_ref()
                    .and_then(|p| p.location.as_ref())
                    .and_then(|l| l.sections.as_deref())
                    .map(|codes| codes.split(',').map(str::to_owned).collect())
                    .unwrap_or_default(),
            })
            .collect();
        let route = map.route(&wanted);
        let location_of = |name: &str| {
            open.iter()
                .find(|item| item.name.as_deref() == Some(name))
                .map(|item| item_view(item)["location"].clone())
                .filter(|location| !location.is_null())
        };
        let stops: Vec<Value> = route
            .stops
            .iter()
            .enumerate()
            .map(|(i, stop)| {
                prune(json!({
                    "stop": i + 1,
                    "items": stop.names,
                    "location": location_of(&stop.names[0]),
                    "walk_metres": stop.metres_from_previous.round(),
                }))
            })
            .collect();
        let mut summary = json!({
            "list": list_summary(&list),
            "stops": stops,
            "total_walk_metres": route.total_metres.round(),
        });
        if !route.unplaced.is_empty() {
            summary["not_on_map"] = json!(route.unplaced);
            summary["note"] = json!(
                "Free-text items and products without a shelf location cannot be placed on the map."
            );
        }
        Ok(RouteMap {
            summary,
            svg: map.svg(&route),
        })
    }

    async fn store_map(&self, store_id: &str) -> Result<Arc<StoreMap>> {
        let mut maps = self.maps.lock().await;
        if let Some(map) = maps.get(store_id) {
            return Ok(map.clone());
        }
        let layer = |name| self.api.store_map_layer(store_id, name, 1);
        let (departments, sections, pois) =
            tokio::try_join!(layer("departments"), layer("sections"), layer("pois"))?;
        let map = Arc::new(
            StoreMap::from_layers(&departments, &sections, &pois)
                .with_context(|| format!("store {store_id} has no usable indoor map"))?,
        );
        maps.insert(store_id.to_owned(), map.clone());
        Ok(map)
    }

    pub async fn search_stores(&self, args: SearchStoresArgs) -> Result<Value> {
        let query = required("query", &args.query)?;
        let brands: Vec<String> = args
            .brands
            .iter()
            .map(|brand| brand.trim().to_uppercase().replace(['-', ' '], "_"))
            .collect();
        if let Some(unknown) = brands.iter().find(|b| !BRANDS.contains(&b.as_str())) {
            bail!("unknown brand {unknown}; use one of {}", BRANDS.join(", "));
        }
        let limit = args.limit.unwrap_or(10).clamp(1, 50);
        let page = self.api.search_stores(query, &brands, limit).await?;
        Ok(prune(json!({
            "total": page["totalCount"],
            "stores": page["stores"],
        })))
    }

    pub async fn get_store(&self, args: StoreArgs) -> Result<Value> {
        let store = self
            .api
            .store(required("store_id", &args.store_id)?)
            .await?;
        Ok(store_detail_view(&store))
    }

    async fn resolve(&self, wanted: Option<&str>) -> Result<ShoppingList> {
        let lists = self.api.shopping_lists().await?;
        pick_list(lists, wanted)
    }
}

fn required<'a>(name: &str, value: &'a str) -> Result<&'a str> {
    let value = value.trim();
    if value.is_empty() {
        bail!("{name} must not be empty");
    }
    Ok(value)
}

fn optional(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

fn clean_names(names: &[String]) -> Vec<String> {
    names
        .iter()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .collect()
}

/// Accepts a share link such as `https://s-ostoslista.fi/liity/<id>` or a
/// bare link id.
fn share_link_id(link: &str) -> Result<String> {
    let link = required("link", link)?;
    if !link.contains('/') {
        return Ok(link.to_owned());
    }
    let url = url::Url::parse(link).context("not a valid link")?;
    let segments: Vec<_> = url
        .path_segments()
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .collect();
    match segments.as_slice() {
        ["liity", id] => Ok((*id).to_owned()),
        _ => bail!("not a share link; expected {SHARE_URL}<id>"),
    }
}

/// Selects a list by id or case-insensitive name, or the only list when none
/// is named.
fn pick_list(lists: Vec<ShoppingList>, wanted: Option<&str>) -> Result<ShoppingList> {
    let names = || {
        lists
            .iter()
            .map(|list| format!("{:?} ({})", list.name, list.id))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let Some(wanted) = optional(wanted) else {
        return match lists.len() {
            0 => bail!("the user has no shopping lists; create one with create_shopping_list"),
            1 => Ok(lists.into_iter().next().expect("one list")),
            _ => bail!("the user has several lists; pass `list`: {}", names()),
        };
    };
    if let Some(index) = lists.iter().position(|list| list.id == wanted) {
        return Ok(lists.into_iter().nth(index).expect("index in range"));
    }
    let wanted_lower = wanted.to_lowercase();
    let matches: Vec<_> = lists
        .iter()
        .enumerate()
        .filter(|(_, list)| list.name.trim().to_lowercase() == wanted_lower)
        .map(|(index, _)| index)
        .collect();
    match matches.as_slice() {
        [index] => Ok(lists.into_iter().nth(*index).expect("index in range")),
        [] => bail!("no list matches {wanted:?}; lists: {}", names()),
        _ => bail!(
            "several lists are named {wanted:?}; pass the id: {}",
            names()
        ),
    }
}

fn require_item<'a>(list: &'a ShoppingList, id: &str) -> Result<&'a Item> {
    match list.items.iter().find(|item| item.id == id) {
        Some(item) => Ok(item),
        None => bail!(
            "no item {id} on list {:?}; read the list for current ids",
            list.name
        ),
    }
}

fn new_items(before: &ShoppingList, after: &ShoppingList) -> Vec<Value> {
    let known: HashSet<_> = before.items.iter().map(|item| item.id.as_str()).collect();
    after
        .items
        .iter()
        .filter(|item| !known.contains(item.id.as_str()))
        .map(item_view)
        .collect()
}

fn list_summary(list: &ShoppingList) -> Value {
    let open = list.items.iter().filter(|item| !item.collected).count();
    prune(json!({
        "id": list.id,
        "name": list.name,
        "store": list.store.as_ref().and_then(|store| store.name.clone()),
        "store_id": list.store_id,
        "shared": list.shared.then_some(true),
        "open_items": open,
        "collected_items": list.items.len() - open,
        "modified_at": list.modified_at,
    }))
}

fn list_detail(list: &ShoppingList, include_collected: bool) -> Value {
    let mut detail = list_summary(list);
    let items: Vec<_> = list
        .items
        .iter()
        .filter(|item| include_collected || !item.collected)
        .map(item_view)
        .collect();
    detail["items"] = Value::Array(items);
    detail
}

fn item_view(item: &Item) -> Value {
    let product = item.product.as_ref();
    let location = product.and_then(|p| p.location.as_ref());
    prune(json!({
        "id": item.id,
        "name": item.name,
        "collected": item.collected,
        "quantity": item.quantity,
        "unit": item.quantity_unit,
        "product_id": item.sok_id,
        "ean": item.ean,
        "price_eur": product
            .and_then(|p| p.pricing.as_ref())
            .and_then(|pricing| pricing.current_price),
        "location": location.map(|l| {
            place(l.aisle.as_deref(), l.label.as_deref(), l.name.as_deref())
        }),
    }))
}

/// Where a product sits in the store. Products in a labelled department,
/// such as MAITO-OSASTO or HEDELMÄT VIHANNEKSET, carry the department's
/// internal code in `aisle` rather than an aisle number, so only the label is
/// shown for them.
fn place(aisle: Option<&str>, department: Option<&str>, shelf: Option<&str>) -> Value {
    prune(json!({
        "aisle": department.is_none().then_some(aisle).flatten(),
        "department": department,
        "shelf": shelf,
    }))
}

fn history_source(recommendations: &Value) -> &'static str {
    match recommendations["historySource"].as_str() {
        Some("BOA") => "card_purchases",
        Some("S_OSTOSLISTA") => "app_checked_items",
        _ => "unknown",
    }
}

/// Search results with the products in `history` moved to the front, each
/// paired with whether it is one of them. The order is otherwise kept.
fn bought_first<'a>(items: &'a [Value], history: Option<&History>) -> Vec<(&'a Value, bool)> {
    let mut marked: Vec<_> = items
        .iter()
        .map(|product| (product, history.is_some_and(|h| h.contains(product))))
        .collect();
    marked.sort_by_key(|(_, bought)| !bought);
    marked
}

fn groups_view(recommendations: &Value, view: fn(&Value) -> Value) -> Vec<Value> {
    recommendations["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|group| {
            json!({
                "category": group["groupName"],
                "products": group["products"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(view)
                    .collect::<Vec<_>>(),
            })
        })
        .collect()
}

/// A slimmer product row for long history listings.
fn history_product_view(product: &Value) -> Value {
    let full = product_view(product);
    let mut view = Map::new();
    for key in [
        "id",
        "name",
        "price_eur",
        "regular_price_eur",
        "campaign_until",
        "available",
    ] {
        if let Some(value) = full.get(key) {
            view.insert(key.to_owned(), value.clone());
        }
    }
    Value::Object(view)
}

/// A compact product row: identity, price, and where to find it.
fn product_view(product: &Value) -> Value {
    let pricing = &product["pricing"];
    let price = pricing["currentPrice"].as_f64();
    let regular = pricing["regularPrice"]
        .as_f64()
        .filter(|r| Some(*r) != price);
    prune(json!({
        "id": product["id"],
        "ean": product["ean"],
        "name": product["name"],
        "brand": product["brandName"],
        "price_eur": price,
        "price_unit": product["priceUnit"],
        "approximate_price": product["approxPrice"].as_bool().filter(|a| *a),
        "regular_price_eur": regular,
        "campaign_until": pricing["campaignPriceValidUntil"],
        "lowest_30_day_price_eur": regular.and(pricing["lowest30DayPrice"].as_f64()),
        "deposit_eur": pricing["depositPrice"],
        "comparison_price_eur": pricing["comparisonPrice"],
        "comparison_unit": product["comparisonUnit"],
        "available": product["available"].as_bool().filter(|a| !a),
        "location": place(
            product["location"]["aisle"].as_str(),
            product["location"]["label"].as_str(),
            product["location"]["name"].as_str(),
        ),
    }))
}

/// Without a store, the API answers for a reference store, whose location
/// means nothing to the user.
fn scoped(mut view: Value, store_id: Option<&str>) -> Value {
    if store_id.is_none()
        && let Some(fields) = view.as_object_mut()
    {
        fields.remove("location");
    }
    view
}

fn product_detail_view(product: &Value) -> Value {
    let mut view = product_view(product);
    // Allergens are grouped by containment level: CONTAINS, MAY_CONTAIN,
    // FREE_FROM.
    let mut allergens = Map::new();
    for allergen in product["allergens"].as_array().into_iter().flatten() {
        let (Some(name), Some(level)) = (
            allergen["allergenTypeText"].as_str(),
            allergen["levelOfContainmentCode"].as_str(),
        ) else {
            continue;
        };
        let names = allergens
            .entry(level.to_lowercase())
            .or_insert_with(|| json!([]));
        if let Some(names) = names.as_array_mut() {
            names.push(json!(name.split_whitespace().collect::<Vec<_>>().join(" ")));
        }
    }
    let extra = prune(json!({
        "description": product["description"],
        "ingredients": product["ingredientStatement"],
        "allergens": allergens,
        "nutrients": product["nutrients"],
        "country_of_origin": product["countryName"]["fi"],
        "supplier": product["supplierName"],
        "frozen": product["frozen"].as_bool().filter(|f| *f),
        "alcohol_age_limit": product["isAgeLimitedByAlcohol"].as_bool().filter(|a| *a),
        "measurement": product["measurement"],
        "labels": product["packagingLabels"],
    }));
    if let (Some(view), Value::Object(extra)) = (view.as_object_mut(), extra) {
        view.extend(extra);
    }
    view
}

fn store_detail_view(store: &Value) -> Value {
    let hours: Vec<_> = store["weeklyOpeningHours"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|week| week["openingTimes"].as_array().into_iter().flatten())
        .map(|day| {
            let ranges: Vec<_> = day["ranges"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| Some(format!("{}-{}", r["open"].as_str()?, r["close"].as_str()?)))
                .collect();
            let hours = match day["mode"].as_str() {
                Some("CLOSED") => "closed".to_owned(),
                Some("ALL_DAY") => "24h".to_owned(),
                Some("BY_RESERVATION") => "by reservation".to_owned(),
                _ => ranges.join(", "),
            };
            prune(json!({
                "date": day["date"],
                "day": day["day"],
                "hours": hours,
                "exceptional": day["exceptional"].as_bool().filter(|e| *e),
                "note": day["message"]["fi"].as_str().or(day["message"]["en"].as_str()),
            }))
        })
        .collect();
    let services: Vec<_> = store["services"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s["name"]["default"].as_str())
        .collect();
    prune(json!({
        "id": store["id"],
        "name": store["name"],
        "brand": store["brand"],
        "street": store["street"],
        "postal_code": store["postalCode"],
        "city": store["city"],
        "phone": store["contactInfo"]["phoneNumber"]["number"],
        "email": store["contactInfo"]["email"],
        "opening_hours": hours,
        "services": services,
    }))
}

/// Drops nulls, empty strings, and empty arrays and objects, recursively, so
/// tool output stays small.
fn prune(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, prune(v)))
                .filter(|(_, v)| !is_empty(v))
                .collect::<Map<_, _>>(),
        ),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(prune)
                .filter(|v| !is_empty(v))
                .collect(),
        ),
        other => other,
    }
}

fn is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(id: &str, name: &str, items: &[(&str, bool)]) -> ShoppingList {
        serde_json::from_value(json!({
            "id": id,
            "name": name,
            "items": items.iter().map(|(id, collected)| json!({
                "id": id, "name": format!("item {id}"), "collected": collected
            })).collect::<Vec<_>>(),
        }))
        .unwrap()
    }

    #[test]
    fn omitted_list_resolves_only_when_unambiguous() {
        let one = vec![list("a", "Ruoka", &[])];
        assert_eq!(pick_list(one, None).unwrap().id, "a");
        let two = vec![list("a", "Ruoka", &[]), list("b", "Mökki", &[])];
        let error = pick_list(two, None).unwrap_err().to_string();
        assert!(
            error.contains("several lists") && error.contains("Mökki"),
            "{error}"
        );
        assert!(pick_list(vec![], None).is_err());
    }

    #[test]
    fn list_is_found_by_id_or_case_insensitive_name() {
        let lists = || vec![list("a", "Ruoka", &[]), list("b", "Mökki", &[])];
        assert_eq!(pick_list(lists(), Some("b")).unwrap().id, "b");
        assert_eq!(pick_list(lists(), Some(" ruoka ")).unwrap().id, "a");
        assert_eq!(pick_list(lists(), Some("MÖKKI")).unwrap().id, "b");
        assert!(pick_list(lists(), Some("Kauppa")).is_err());
        let duplicates = vec![list("a", "Ruoka", &[]), list("b", "ruoka", &[])];
        let error = pick_list(duplicates, Some("Ruoka"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("pass the id"), "{error}");
    }

    #[test]
    fn detail_can_hide_collected_items_and_summary_counts_both() {
        let list = list("a", "Ruoka", &[("x", false), ("y", true)]);
        let detail = list_detail(&list, false);
        assert_eq!(detail["open_items"], 1);
        assert_eq!(detail["collected_items"], 1);
        assert_eq!(detail["items"].as_array().unwrap().len(), 1);
        assert_eq!(detail["items"][0]["id"], "x");
    }

    #[test]
    fn unknown_item_ids_are_rejected_before_any_change() {
        let list = list("a", "Ruoka", &[("x", false)]);
        assert!(require_item(&list, "x").is_ok());
        let error = require_item(&list, "zzz").unwrap_err().to_string();
        assert!(error.contains("zzz"), "{error}");
    }

    #[test]
    fn share_links_parse_from_url_or_bare_id() {
        assert_eq!(
            share_link_id("https://s-ostoslista.fi/liity/abc-123").unwrap(),
            "abc-123"
        );
        assert_eq!(
            share_link_id("https://www.s-ostoslista.fi/liity/abc/").unwrap(),
            "abc"
        );
        assert_eq!(share_link_id(" abc ").unwrap(), "abc");
        assert!(share_link_id("https://s-ostoslista.fi/ostoslistaan/abc").is_err());
        assert!(share_link_id("https://s-ostoslista.fi/liity/").is_err());
        assert!(share_link_id("").is_err());
    }

    #[test]
    fn product_view_reports_campaigns_and_hides_defaults() {
        let product = json!({
            "id": "1", "ean": "2", "name": "Kahvi", "brandName": null, "available": true,
            "approxPrice": false, "priceUnit": "KPL", "comparisonUnit": "KGM",
            "pricing": { "currentPrice": 5.99, "regularPrice": 8.49, "comparisonPrice": 11.98,
                "campaignPrice": 5.99, "campaignPriceValidUntil": "2026-11-30",
                "lowest30DayPrice": 7.99, "depositPrice": null },
            "location": { "aisle": "12", "label": null, "name": "Kahvi" }
        });
        let view = product_view(&product);
        assert_eq!(view["regular_price_eur"], 8.49);
        assert_eq!(view["campaign_until"], "2026-11-30");
        assert_eq!(view["lowest_30_day_price_eur"], 7.99);
        assert_eq!(view["location"], json!({ "aisle": "12", "shelf": "Kahvi" }));
        for hidden in ["brand", "available", "approximate_price", "deposit_eur"] {
            assert!(view.get(hidden).is_none(), "{hidden}: {view}");
        }

        let mut regular = product.clone();
        regular["pricing"]["regularPrice"] = json!(5.99);
        regular["available"] = json!(false);
        let view = product_view(&regular);
        assert!(view.get("regular_price_eur").is_none());
        assert!(view.get("lowest_30_day_price_eur").is_none());
        assert_eq!(view["available"], false);
    }

    #[test]
    fn department_replaces_its_internal_aisle_code() {
        assert_eq!(
            place(Some("1006"), Some("MAITO-OSASTO"), Some("Ruokajuomat")),
            json!({ "department": "MAITO-OSASTO", "shelf": "Ruokajuomat" })
        );
        assert_eq!(place(None, None, None), json!({}));
    }

    #[test]
    fn history_lists_slim_products_by_category() {
        let history = json!({
            "historySource": "BOA",
            "groups": [{
                "groupName": "Juustot",
                "products": [{
                    "id": "100008668",
                    "name": "Kotimaista raejuusto",
                    "pricing": { "currentPrice": 1.19 },
                    "location": { "aisle": "12", "label": "Juustot" }
                }]
            }]
        });
        assert_eq!(history_source(&history), "card_purchases");
        assert_eq!(
            groups_view(&history, history_product_view),
            vec![json!({
                "category": "Juustot",
                "products": [{ "id": "100008668", "name": "Kotimaista raejuusto", "price_eur": 1.19 }]
            })]
        );
        assert_eq!(
            history_source(&json!({ "historySource": "S_OSTOSLISTA" })),
            "app_checked_items"
        );
    }

    #[test]
    fn bought_products_move_to_the_front_in_search_order() {
        let history = History::new(json!({
            "groups": [{ "products": [{ "id": "3" }, { "id": "5" }] }]
        }));
        let items: Vec<_> = (1..=5).map(|id| json!({ "id": id.to_string() })).collect();
        let order: Vec<_> = bought_first(&items, Some(&history))
            .into_iter()
            .map(|(product, bought)| (product["id"].as_str().unwrap().to_owned(), bought))
            .collect();
        let expected = [
            ("3", true),
            ("5", true),
            ("1", false),
            ("2", false),
            ("4", false),
        ];
        assert_eq!(order, expected.map(|(id, b)| (id.to_owned(), b)));
        assert!(bought_first(&items, None).iter().all(|(_, bought)| !bought));
    }

    #[test]
    fn allergens_are_grouped_by_containment() {
        let view = product_detail_view(&json!({
            "id": "1",
            "allergens": [
                { "allergenTypeText": "Maito", "levelOfContainmentCode": "CONTAINS" },
                { "allergenTypeText": "Gluteenia  sisältävät", "levelOfContainmentCode": "FREE_FROM" },
                { "allergenTypeText": "Muna", "levelOfContainmentCode": "FREE_FROM" },
                { "allergenTypeText": "Seesami", "levelOfContainmentCode": null }
            ]
        }));
        assert_eq!(
            view["allergens"],
            json!({ "contains": ["Maito"], "free_from": ["Gluteenia sisältävät", "Muna"] })
        );
    }

    #[test]
    fn store_hours_render_ranges_and_closed_days() {
        let store = json!({
            "id": "1", "name": "Prisma",
            "weeklyOpeningHours": [{ "openingTimes": [
                { "date": "2026-11-23", "day": "MON", "mode": "RANGE",
                  "ranges": [{ "open": "06:00", "close": "00:00" }] },
                { "date": "2026-12-25", "day": "FRI", "mode": "CLOSED", "exceptional": true,
                  "message": { "fi": "Joulupäivä" } }
            ]}],
            "services": [{ "name": { "default": "Kahvila" } }]
        });
        let view = store_detail_view(&store);
        assert_eq!(view["opening_hours"][0]["hours"], "06:00-00:00");
        assert_eq!(view["opening_hours"][1]["hours"], "closed");
        assert_eq!(view["opening_hours"][1]["note"], "Joulupäivä");
        assert_eq!(view["services"][0], "Kahvila");
    }

    #[test]
    fn prune_drops_empty_values_recursively() {
        let value = prune(
            json!({ "a": null, "b": "", "c": [null, {}], "d": { "e": null },
            "f": 0, "g": false, "h": "x" }),
        );
        assert_eq!(value, json!({ "f": 0, "g": false, "h": "x" }));
    }
}
