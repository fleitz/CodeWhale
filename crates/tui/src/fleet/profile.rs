//! Fleet profile vocabulary and config-facing type aliases.

#![allow(dead_code)]

#[allow(unused_imports)]
pub use codewhale_config::{
    FleetDelegationHints, FleetLoadout, FleetProfile, FleetProfilePermissions, FleetRole, FleetSlot,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fleet_profile_round_trips_through_serde_with_safe_defaults() {
        let profile = FleetProfile::default();

        let serialized = toml::to_string(&profile).expect("profile serializes");
        let round_tripped: FleetProfile =
            toml::from_str(&serialized).expect("profile deserializes");

        assert_eq!(round_tripped, profile);
        assert_eq!(round_tripped.role.name, "general");
        assert_eq!(round_tripped.loadout, FleetLoadout::Inherit);
        assert!(!round_tripped.permissions.allow_shell);
        assert!(!round_tripped.permissions.trust);
        assert!(round_tripped.permissions.approval_required);
        assert_eq!(round_tripped.delegation.max_spawn_depth, None);
        assert_eq!(round_tripped.delegation.max_concurrency, None);
    }

    #[test]
    fn fleet_profile_explicit_toml_parses_role_loadout_permissions() {
        let profile: FleetProfile = toml::from_str(
            r#"
slot = "reviewer"
loadout = "deep-reasoning"

[role]
name = "verifier"
instructions = "Review the patch and produce verification evidence."

[permissions]
allow_shell = true
trust = true
approval_required = false

[delegation]
max_spawn_depth = 1
concurrency = 2
"#,
        )
        .expect("explicit fleet profile parses");

        assert_eq!(profile.slot, FleetSlot::Reviewer);
        assert_eq!(profile.role.name, "verifier");
        assert_eq!(
            profile.role.instructions.as_deref(),
            Some("Review the patch and produce verification evidence.")
        );
        assert_eq!(profile.loadout, FleetLoadout::DeepReasoning);
        assert!(profile.permissions.allow_shell);
        assert!(profile.permissions.trust);
        assert!(!profile.permissions.approval_required);
        assert_eq!(profile.delegation.max_spawn_depth, Some(1));
        assert_eq!(profile.delegation.max_concurrency, Some(2));
    }

    #[test]
    fn fleet_profile_accepts_compact_role_string() {
        let profile: FleetProfile = toml::from_str(
            r#"
role = "scout"
loadout = "fast"
"#,
        )
        .expect("compact fleet profile parses");

        assert_eq!(profile.role.name, "scout");
        assert_eq!(profile.loadout, FleetLoadout::Fast);
        assert_eq!(profile.permissions, FleetProfilePermissions::default());
    }
}
