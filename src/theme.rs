//! Material role generation belongs to the daemon, not the QML render thread.
use std::collections::BTreeMap;

use material_colors::{
    color::Argb,
    dynamic_color::{DynamicScheme, Variant},
    palette::TonalPalette,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct ThemeRoles(pub BTreeMap<String, String>);

impl JsonSchema for ThemeRoles {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ThemeRoles".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let names: Vec<_> = Self::default().0.into_keys().collect();
        let properties: serde_json::Map<String, serde_json::Value> = names
            .iter()
            .map(|name| {
                (
                    name.clone(),
                    serde_json::json!({"type":"string", "pattern":"^#[0-9a-fA-F]{6}$"}),
                )
            })
            .collect();
        schemars::json_schema!({"type":"object", "properties":properties, "required":names, "additionalProperties":false})
    }
}

pub(crate) fn reply(roles: &ThemeRoles) -> serde_json::Value {
    crate::protocol::success(serde_json::json!({"theme":roles}))
}

impl Default for ThemeRoles {
    fn default() -> Self {
        scheme(Argb::from_u32(0xff6750a4), false)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ThemeRequest {
    /// Opaque sRGB seed, exactly #rrggbb.
    #[schemars(regex(pattern = "^#[0-9a-fA-F]{6}$"))]
    seed: String,
    dark: bool,
}

impl ThemeRequest {
    pub(crate) fn resolve(&self) -> Result<ThemeRoles, &'static str> {
        if self.seed.len() != 7
            || !self.seed.starts_with('#')
            || !self.seed[1..].bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err("seed must be an opaque #rrggbb color");
        }
        let rgb = u32::from_str_radix(&self.seed[1..], 16).map_err(|_| "invalid seed")?;
        Ok(scheme(Argb::from_u32(0xff000000 | rgb), self.dark))
    }
}

pub(crate) fn scheme(seed: Argb, dark: bool) -> ThemeRoles {
    let scheme = DynamicScheme::by_variant(seed, &Variant::TonalSpot, dark, Some(0.0));
    let mut roles = BTreeMap::new();
    macro_rules! role {
        ($($name:literal => $method:ident),* $(,)?) => {$(
            roles.insert($name.into(), scheme.$method().to_hex_with_pound());
        )*};
    }
    role! {
        "primary" => primary, "onPrimary" => on_primary,
        "primaryContainer" => primary_container, "onPrimaryContainer" => on_primary_container,
        "secondary" => secondary, "onSecondary" => on_secondary,
        "secondaryContainer" => secondary_container, "onSecondaryContainer" => on_secondary_container,
        "tertiary" => tertiary, "onTertiary" => on_tertiary,
        "tertiaryContainer" => tertiary_container, "onTertiaryContainer" => on_tertiary_container,
        "surface" => surface, "surfaceContainerLowest" => surface_container_lowest,
        "surfaceContainerLow" => surface_container_low, "surfaceContainer" => surface_container,
        "surfaceContainerHigh" => surface_container_high, "surfaceContainerHighest" => surface_container_highest,
        "onSurface" => on_surface, "onSurfaceVariant" => on_surface_variant,
        "outline" => outline, "outlineVariant" => outline_variant,
        "error" => error, "onError" => on_error,
        "errorContainer" => error_container, "onErrorContainer" => on_error_container,
    }
    // MCU JS 0.4.0's pinned 2021 spec uses tone 30 (not this port's older
    // tone 10) for light container text. Keep the established role contract.
    for (name, palette) in [
        ("onPrimaryContainer", &scheme.primary_palette),
        ("onSecondaryContainer", &scheme.secondary_palette),
        ("onTertiaryContainer", &scheme.tertiary_palette),
        ("onErrorContainer", &scheme.error_palette),
    ] {
        roles.insert(
            name.into(),
            palette.tone(if dark { 90 } else { 30 }).to_hex_with_pound(),
        );
    }
    for (name, foreground, hue) in [
        ("success", "onSuccess", 145.0),
        ("warning", "onWarning", 85.0),
    ] {
        let palette = TonalPalette::from_hue_and_chroma(hue, 36.0);
        roles.insert(
            name.into(),
            palette.tone(if dark { 80 } else { 40 }).to_hex_with_pound(),
        );
        roles.insert(
            foreground.into(),
            palette
                .tone(if dark { 20 } else { 100 })
                .to_hex_with_pound(),
        );
    }
    ThemeRoles(roles)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_javascript_2021_roles() {
        let fixtures: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../test_support/theme-2021.json")).unwrap();
        for fixture in fixtures {
            let request: ThemeRequest = serde_json::from_value(
                serde_json::json!({"seed":fixture["seed"], "dark":fixture["dark"]}),
            )
            .unwrap();
            assert_eq!(
                serde_json::to_value(request.resolve().unwrap()).unwrap(),
                fixture["roles"],
                "{} dark={}",
                request.seed,
                request.dark
            );
        }
    }

    #[test]
    fn invalid_seeds_fail_closed() {
        for seed in ["", "#fff", "#gg0000", "#ff000000", "red", "#💥ab"] {
            assert!(
                ThemeRequest {
                    seed: seed.into(),
                    dark: false
                }
                .resolve()
                .is_err()
            );
        }
        assert!(
            serde_json::from_value::<ThemeRequest>(serde_json::json!({"seed":"#ffffff", "dark":1}))
                .is_err()
        );
    }
}
