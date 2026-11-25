//! Links that hand products to the S-kaupat online store.
//!
//! The link opens a page listing the products with their prices and a button
//! that adds them all to the cart. The S-ostoslista app shares a list with
//! S-kaupat this way: the products are serialized as repeated `id` form
//! fields, deflated without a zlib header, and base64-encoded into the path of
//! `/ostoskoriin/`.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use url::Url;
use url::form_urlencoded;

const CART_URL: &str = "https://www.s-kaupat.fi/ostoskoriin/";

/// A product to offer for the cart.
#[derive(Clone, Debug, PartialEq)]
pub struct CartItem {
    /// The product id (`sokId`).
    pub id: String,
    pub quantity: f64,
    /// The quantity's unit, such as KPL or KG.
    pub unit: Option<String>,
}

/// The s-kaupat.fi address that lists `items` under `title`, ready to add to
/// the cart.
pub fn cart_link(title: &str, items: &[CartItem]) -> String {
    let payload = STANDARD.encode(miniz_oxide::deflate::compress_to_vec(
        product_list(items).as_bytes(),
        9,
    ));
    let path: String = form_urlencoded::byte_serialize(payload.as_bytes()).collect();
    let mut url =
        Url::parse(&format!("{CART_URL}{path}")).expect("the encoded cart is a valid path segment");
    url.query_pairs_mut().append_pair("title", title);
    url.into()
}

/// One `id` field per product, holding `<id>_<quantity>_<unit>`. Quantities
/// carry at most three decimals, and an underscore in an id is escaped so it
/// cannot be read as a separator.
fn product_list(items: &[CartItem]) -> String {
    let mut fields = form_urlencoded::Serializer::new(String::new());
    for item in items {
        let mut value = item.id.replace('_', "%5f");
        value.push_str(&format!("_{}", (item.quantity * 1000.0).round() / 1000.0));
        if let Some(unit) = &item.unit {
            value.push_str(&format!("_{unit}"));
        }
        fields.append_pair("id", &value);
    }
    fields.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, quantity: f64, unit: Option<&str>) -> CartItem {
        CartItem {
            id: id.to_owned(),
            quantity,
            unit: unit.map(str::to_owned),
        }
    }

    #[test]
    fn products_serialize_like_the_app() {
        let items = [
            item("100013230", 1.0, Some("KPL")),
            item("100616383", 0.6004, Some("KG")),
            item("100528874", 2.0, None),
        ];
        assert_eq!(
            product_list(&items),
            "id=100013230_1_KPL&id=100616383_0.6_KG&id=100528874_2"
        );
    }

    #[test]
    fn link_decodes_back_to_the_products() {
        let items = [
            item("100013230", 1.0, Some("KPL")),
            item("100528874", 2.0, Some("KPL")),
        ];
        let link = Url::parse(&cart_link("Viikon ruoat", &items)).unwrap();
        assert_eq!(link.host_str(), Some("www.s-kaupat.fi"));
        assert_eq!(
            link.query_pairs().next().unwrap(),
            ("title".into(), "Viikon ruoat".into())
        );

        let segment = link.path().strip_prefix("/ostoskoriin/").unwrap();
        let payload: String = form_urlencoded::parse(format!("p={segment}").as_bytes())
            .next()
            .unwrap()
            .1
            .into_owned();
        // A `+` in the base64 would have been decoded as a space above, so
        // the path must not contain one unescaped.
        assert!(!segment.contains('+') && !segment.contains('/'));
        let deflated = STANDARD.decode(payload).unwrap();
        let text = miniz_oxide::inflate::decompress_to_vec(&deflated).unwrap();
        assert_eq!(
            String::from_utf8(text).unwrap(),
            "id=100013230_1_KPL&id=100528874_2_KPL"
        );
    }
}
