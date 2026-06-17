## Summary

All write paths that rewrite `config.toml`, `settings.toml`, or `tui.toml` now
merge the freshly serialised output with the original file's comments via
`toml_edit`, so user annotations and commented-out keys survive CLI and TUI
config edits.

Previously every path used `toml::to_string_pretty` to re-serialise the
deserialised struct, which unconditionally discards comments.  The fix wraps
the serialised body through `merge_and_preserve_comments` in `codewhale-config`,
which parses both the new body and the original file as `toml_edit::DocumentMut`
and copies three layers of decor: trailing text (file-footer comments), root
table decor (header comments), and per-key leaf decor (inline key comments).

**Changes** (3 files):

| File | Change |
|------|--------|
| `crates/config/src/lib.rs` | `ConfigStore` stores `original_raw: Option<String>`; `save()` calls `merge_and_preserve_comments` (new public fn); 3 new tests |
| `crates/tui/src/config_persistence.rs` | 5 `persist_*` helpers keep the original raw text and write through `save_toml_preserving_comments`; 1 new test |
| `crates/tui/src/settings.rs` | `Settings::save()` and `TuiPrefs::save()` merge comments before writing; 2 new tests |

No new dependencies — `toml_edit` is already a workspace dep and stays internal
to `codewhale-config`.

## Testing

- [x] `cargo fmt --all -- --check`
- [x] `cargo clippy --workspace --all-targets --all-features`
- [x] `cargo test --workspace --all-features`

6 new comment-preservation tests (all passing):

- `config_store_save_preserves_comments` — single key + prefix/suffix comments
- `config_store_save_preserves_disabled_keys` — commented-out key survives
- `config_store_save_preserves_comments_with_other_keys` — multi-key file with comments
- `persist_bool_key_preserves_comments` — TUI persist path
- `tui_prefs_save_preserves_comments` — `tui.toml` path
- `settings_save_preserves_comments` — `settings.toml` path

## Checklist

- [x] Updated docs or comments as needed
- [x] Added or updated tests where relevant
- [x] Verified TUI behavior manually if UI changes
- [ ] Harvested/co-authored credit uses a GitHub numeric noreply address
