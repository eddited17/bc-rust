//! A numeric field's `MinValue` / `MaxValue` properties. BC checks them when a value is entered on a
//! page (TestPage SetValue, the client); `Validate(Field, Value)` and assignments from code do not
//! (the W1 DemoTool validates "Sales Cycle Stage" (MinValue = 1) with 0). The message is the
//! platform's ("The value must be greater than or equal to 0. Value: -5.", as BCApps tests expect).

use al_model::{FieldDef, FieldType};
use rust_decimal::Decimal;

use crate::value::Val;

fn bound(fd: &FieldDef, prop: &str) -> Option<Decimal> {
    crate::record::parse_num(fd.prop_src.get(prop)?.trim())
}

fn shown(d: Decimal) -> String {
    crate::builtins::format_val(&Val::Dec(d), 0)
}

/// Err(message) when `v` is outside the field's MinValue..MaxValue.
pub fn check(fd: &FieldDef, v: &Val) -> Result<(), String> {
    if !matches!(fd.ty, FieldType::Integer | FieldType::BigInteger | FieldType::Decimal) {
        return Ok(());
    }
    if fd.prop_src.get("minvalue").is_none() && fd.prop_src.get("maxvalue").is_none() {
        return Ok(());
    }
    let x = v.as_dec();
    if let Some(min) = bound(fd, "minvalue") {
        if x < min {
            return Err(format!("The value must be greater than or equal to {}. Value: {}.", shown(min), shown(x)));
        }
    }
    if let Some(max) = bound(fd, "maxvalue") {
        if x > max {
            return Err(format!("The value must be less than or equal to {}. Value: {}.", shown(max), shown(x)));
        }
    }
    Ok(())
}
