//! Tool implementations, independent of the MCP transport.

use std::collections::HashSet;

use anyhow::{Result, bail};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::{Api, Item, ItemChanges, ShoppingList};

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
pub struct SearchStoresArgs {
    /// One search word, such as a store name part, district, or city, e.g.
    /// "kalasatama" or "prisma". Multi-word queries often match nothing.
    pub query: String,
    /// Maximum stores to return, 1-50. Defaults to 10.
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateListArgs {
    /// Name for the new list.
    pub name: String,
    /// Store id from search_stores. The list is sorted by this store's aisles.
    pub store_id: String,
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
pub struct RemoveItemsArgs {
    /// List id or name. May be omitted when the user has only one list.
    pub list: Option<String>,
    /// Ids of the items to delete, from get_shopping_list.
    pub item_ids: Vec<String>,
}

pub struct Tools {
    api: Api,
}

impl Tools {
    pub fn new(api: Api) -> Self {
        Self { api }
    }

    pub async fn list_shopping_lists(&self) -> Result<Value> {
        let lists = self.api.shopping_lists().await?;
        Ok(json!({ "lists": lists.iter().map(list_summary).collect::<Vec<_>>() }))
    }

    pub async fn get_shopping_list(&self, args: GetListArgs) -> Result<Value> {
        let list = self.resolve(args.list.as_deref()).await?;
        let list = self.api.shopping_list(&list.id).await?;
        Ok(list_detail(&list, args.include_collected.unwrap_or(true)))
    }

    pub async fn search_stores(&self, args: SearchStoresArgs) -> Result<Value> {
        let query = args.query.trim();
        if query.is_empty() {
            bail!("query must not be empty");
        }
        let limit = args.limit.unwrap_or(10).clamp(1, 50);
        let stores = self.api.search_stores(query, limit).await?;
        Ok(json!({ "stores": stores }))
    }

    pub async fn create_shopping_list(&self, args: CreateListArgs) -> Result<Value> {
        let name = args.name.trim();
        if name.is_empty() {
            bail!("name must not be empty");
        }
        let list = self.api.create_list(name, args.store_id.trim()).await?;
        Ok(json!({ "list": list_summary(&list) }))
    }

    pub async fn add_items(&self, args: AddItemsArgs) -> Result<Value> {
        let names: Vec<String> = args
            .items
            .iter()
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .collect();
        if names.is_empty() {
            bail!("items must contain at least one non-empty name");
        }
        let before = self.resolve(args.list.as_deref()).await?;
        let known: HashSet<_> = before.items.iter().map(|item| item.id.as_str()).collect();
        let after = self.api.add_items(&before.id, &names).await?;
        let added: Vec<_> = after
            .items
            .iter()
            .filter(|item| !known.contains(item.id.as_str()))
            .map(item_view)
            .collect();
        Ok(json!({ "list": list_summary(&after), "added": added }))
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

    async fn resolve(&self, wanted: Option<&str>) -> Result<ShoppingList> {
        let lists = self.api.shopping_lists().await?;
        pick_list(lists, wanted)
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
    let Some(wanted) = wanted.map(str::trim).filter(|w| !w.is_empty()) else {
        return match lists.len() {
            0 => bail!("the user has no shopping lists"),
            1 => Ok(lists.into_iter().next().expect("one list")),
            _ => bail!("the user has several lists; pass `list`: {}", names()),
        };
    };
    if let Some(index) = lists.iter().position(|list| list.id == wanted) {
        return Ok(lists.into_iter().nth(index).expect("index in range"));
    }
    let matches: Vec<_> = lists
        .iter()
        .enumerate()
        .filter(|(_, list)| list.name.trim().eq_ignore_ascii_case(wanted))
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

fn list_summary(list: &ShoppingList) -> Value {
    let open = list.items.iter().filter(|item| !item.collected).count();
    json!({
        "id": list.id,
        "name": list.name,
        "store": list.store.as_ref().and_then(|store| store.name.clone()),
        "open_items": open,
        "collected_items": list.items.len() - open,
        "modified_at": list.modified_at,
    })
}

fn list_detail(list: &ShoppingList, include_collected: bool) -> Value {
    let mut summary = list_summary(list);
    let items: Vec<_> = list
        .items
        .iter()
        .filter(|item| include_collected || !item.collected)
        .map(item_view)
        .collect();
    summary["items"] = Value::Array(items);
    summary
}

fn item_view(item: &Item) -> Value {
    let mut view = json!({
        "id": item.id,
        "name": item.name,
        "collected": item.collected,
    });
    if let Some(quantity) = item.quantity {
        view["quantity"] = json!(quantity);
    }
    if let Some(unit) = &item.quantity_unit {
        view["unit"] = json!(unit);
    }
    if let Some(ean) = &item.ean {
        view["ean"] = json!(ean);
    }
    let price = item
        .product
        .as_ref()
        .and_then(|product| product.pricing.as_ref())
        .and_then(|pricing| pricing.current_price);
    if let Some(price) = price {
        view["price_eur"] = json!(price);
    }
    view
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
        assert!(
            require_item(&list, "zzz")
                .unwrap_err()
                .to_string()
                .contains("zzz")
        );
    }
}
