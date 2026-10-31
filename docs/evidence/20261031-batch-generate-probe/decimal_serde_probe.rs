use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug)]
struct R { unit_cost: Option<Decimal> }

fn main() {
    for body in [r#"{"unit_cost": 12.34}"#, r#"{"unit_cost": "12.34"}"#, r#"{}"#, r#"{"unit_cost": 0}"#] {
        match serde_json::from_str::<R>(body) {
            Ok(v) => println!("OK  {body} -> {:?}", v.unit_cost),
            Err(e) => println!("ERR {body} -> {e}"),
        }
    }
    // serialize back
    let v: R = serde_json::from_str(r#"{"unit_cost":12.345678}"#).unwrap();
    println!("SER -> {}", serde_json::to_string(&v).unwrap());
}
