//! Formats the restore preview and architecture-mismatch warning shown before
//! confirmation prompts. Pure formatting: no I/O — callers write the result.

use comfy_table::{Cell, ContentArrangement, Table, presets::UTF8_FULL_CONDENSED};

use crate::domain::preview::RestorePreview;

#[derive(Clone, Copy)]
enum Tone {
    Warn,
    Bad,
}

fn table(color: bool) -> Table {
    let mut table = Table::new();
    table
        .load_style(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic);
    if color {
        // Tests (and piped output) aren't a tty; without this, comfy-table
        // silently drops the `.fg(..)` styling applied to cells below.
        table.enforce_styling();
    }
    table
}

/// Colours a table cell via comfy-table's own styling instead of embedding raw
/// ANSI in the cell text, which would corrupt comfy-table's column-width
/// measurement. Same approach as `presentation::human`.
fn cell(text: impl Into<String>, tone: Tone, color: bool) -> Cell {
    let cell = Cell::new(text.into());
    if !color {
        return cell;
    }
    match tone {
        Tone::Warn => cell.fg(comfy_table::Color::Yellow),
        Tone::Bad => cell.fg(comfy_table::Color::Red),
    }
}

/// Everything a restore would do, shown before `Proceed? [y/N]`.
pub fn format_restore_preview(preview: &RestorePreview, color: bool) -> String {
    let mismatch = preview.backup_platform.is_known()
        && !preview.backup_platform.matches(&preview.target_platform);
    let target_value = if mismatch {
        format!("{} (mismatch)", preview.target_platform)
    } else {
        preview.target_platform.to_string()
    };
    let created_at = preview
        .created_at
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();

    let mut table = table(color);
    table.add_row(vec![
        Cell::new("Source"),
        Cell::new(preview.source.display().to_string()),
    ]);
    table.add_row(vec![Cell::new("Created at"), Cell::new(created_at)]);
    table.add_row(vec![
        Cell::new("Backup platform"),
        Cell::new(preview.backup_platform.to_string()),
    ]);
    table.add_row(vec![
        Cell::new("Target platform"),
        if mismatch {
            cell(target_value, Tone::Warn, color)
        } else {
            Cell::new(target_value)
        },
    ]);
    table.add_row(vec![
        Cell::new("Volumes"),
        Cell::new(format!(
            "{} to create, {} to overwrite, {} skipped",
            preview.volumes_to_create, preview.volumes_to_overwrite, preview.volumes_skipped
        )),
    ]);
    table.add_row(vec![
        Cell::new("Images"),
        Cell::new(format!("{} to load", preview.images_to_load)),
    ]);
    table.add_row(vec![
        Cell::new("Containers"),
        Cell::new(format!("{} to import", preview.containers_to_import)),
    ]);
    table.add_row(vec![
        Cell::new("Overwrite"),
        Cell::new(if preview.overwrite { "on" } else { "off" }),
    ]);

    format!("Restore preview\n{table}")
}

/// Lists the images/containers built for another platform, shown before
/// `Are you sure? [y/N]`. Only meaningful when `preview.mismatches` is non-empty.
pub fn format_arch_warning(preview: &RestorePreview, color: bool) -> String {
    let mut table = table(color);
    table.add_row(vec![Cell::new(format!(
        "{} item(s) were built for another platform than this daemon ({}):",
        preview.mismatches.len(),
        preview.target_platform
    ))]);
    for item in &preview.mismatches {
        let line = format!(
            "  {} {}  ({})",
            item.kind.to_string().to_lowercase(),
            item.name,
            item.platform
        );
        table.add_row(vec![cell(line, Tone::Warn, color)]);
    }
    let closing = if preview.force_arch_mismatch {
        Cell::new(
            "--force-import-if-arch-mismatch is set: they will be imported anyway and may not run.",
        )
    } else {
        cell(
            format!(
                "{} image(s)/container(s) will be skipped. Pass --force-import-if-arch-mismatch to import them anyway.",
                preview.skipped_for_arch()
            ),
            Tone::Bad,
            color,
        )
    };
    table.add_row(vec![closing]);

    format!("Architecture mismatch\n{table}")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use time::macros::datetime;

    use super::*;
    use crate::domain::platform::Platform;
    use crate::domain::preview::{MismatchedItem, RestorePreview};
    use crate::domain::refs::ItemKind;

    fn preview() -> RestorePreview {
        RestorePreview {
            source: PathBuf::from("/backups/out"),
            created_at: datetime!(2026-09-19 14:03:11 UTC),
            backup_platform: Platform::new("linux", "amd64"),
            target_platform: Platform::new("linux", "arm64"),
            overwrite: false,
            force_arch_mismatch: false,
            volumes_to_create: 1,
            volumes_to_overwrite: 0,
            volumes_skipped: 1,
            images_to_load: 1,
            containers_to_import: 0,
            mismatches: vec![MismatchedItem {
                kind: ItemKind::Image,
                name: "app:latest".into(),
                platform: Platform::new("linux", "arm64"),
            }],
        }
    }

    #[test]
    fn restore_preview_shows_summary_and_mismatch() {
        let text = format_restore_preview(&preview(), false);
        assert!(text.contains("Restore preview"));
        assert!(text.contains("linux/amd64"));
        assert!(text.contains("linux/arm64 (mismatch)"));
        assert!(text.contains("1 to create, 0 to overwrite, 1 skipped"));
        assert!(text.contains("1 to load"));
    }

    #[test]
    fn restore_preview_omits_mismatch_marker_for_case_and_variant_only_differences() {
        let mut same = preview();
        same.backup_platform = Platform {
            os: "linux".into(),
            arch: "arm64".into(),
            variant: "v8".into(),
        };
        same.target_platform = Platform::new("Linux", "ARM64");
        let text = format_restore_preview(&same, false);
        assert!(!text.contains("(mismatch)"));
    }

    #[test]
    fn restore_preview_omits_mismatch_marker_for_unknown_backup_platform() {
        let mut unknown = preview();
        unknown.backup_platform = Platform::default();
        unknown.mismatches = Vec::new();
        let text = format_restore_preview(&unknown, false);
        assert!(text.contains("unknown"));
        assert!(
            !text.contains("(mismatch)"),
            "an unknown backup platform can't prove a mismatch"
        );
    }

    #[test]
    fn restore_preview_color_toggle() {
        assert!(format_restore_preview(&preview(), true).contains("\x1b["));
        assert!(!format_restore_preview(&preview(), false).contains("\x1b["));
    }

    #[test]
    fn arch_warning_lists_mismatch_and_skip_notice() {
        let text = format_arch_warning(&preview(), false);
        assert!(text.contains("Architecture mismatch"));
        assert!(text.contains("image app:latest"));
        assert!(text.contains("will be skipped"));
    }

    #[test]
    fn arch_warning_forced_mentions_import_anyway() {
        let mut forced = preview();
        forced.force_arch_mismatch = true;
        let text = format_arch_warning(&forced, false);
        assert!(text.contains("imported anyway"));
    }

    #[test]
    fn arch_warning_color_toggle() {
        assert!(format_arch_warning(&preview(), true).contains("\x1b["));
        assert!(!format_arch_warning(&preview(), false).contains("\x1b["));
    }
}
