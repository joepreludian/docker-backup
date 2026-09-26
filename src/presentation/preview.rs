//! Formats the restore preview and architecture-mismatch warning shown before
//! confirmation prompts. Pure formatting: no I/O — callers write the result.

use comfy_table::{Cell, ContentArrangement, Table, presets::UTF8_FULL_CONDENSED};
use time::UtcOffset;
use time::macros::format_description;

use crate::domain::preview::{RestorePreview, VolumeOverwritePrompt};

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
    if let Some(compose) = &preview.compose {
        let project = match &compose.backup_project {
            Some(from) if *from != compose.project => {
                format!("{} (backup made from {from})", compose.project)
            }
            Some(_) => compose.project.clone(),
            None => format!("{} (from a full backup)", compose.project),
        };
        table.add_row(vec![Cell::new("Compose project"), Cell::new(project)]);
        if !compose.remaps.is_empty() {
            let lines: Vec<String> = compose
                .remaps
                .iter()
                .map(|r| {
                    format!(
                        "{} {} → {}",
                        r.kind.to_string().to_lowercase(),
                        r.from,
                        r.to
                    )
                })
                .collect();
            table.add_row(vec![Cell::new("Remapped"), Cell::new(lines.join("\n"))]);
        }
        if !compose.not_in_backup.is_empty() {
            table.add_row(vec![
                Cell::new("Not in backup"),
                Cell::new(compose.not_in_backup.join(", ")),
            ]);
        }
    }
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

/// The one-line question asked before `restore-volume --overwrite` empties a volume.
pub fn format_volume_overwrite_prompt(prompt: &VolumeOverwritePrompt) -> String {
    let backed_up = prompt
        .created_at
        .to_offset(UtcOffset::UTC)
        .format(format_description!("[year]-[month]-[day] [hour]:[minute]"))
        .unwrap_or_default();
    format!(
        "Volume {} will be emptied and refilled from {} (backed up {backed_up} UTC). Continue? [y/N] ",
        prompt.target,
        prompt.source.display()
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use time::macros::datetime;

    use super::*;
    use crate::domain::platform::Platform;
    use crate::domain::preview::{
        ComposePreview, MismatchedItem, Remap, RestorePreview, VolumeOverwritePrompt,
    };
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
            compose: None,
        }
    }

    #[test]
    fn preview_lists_the_compose_project_remaps_and_missing_volumes() {
        let mut p = preview();
        p.compose = Some(ComposePreview {
            project: "shop2".into(),
            backup_project: Some("shop".into()),
            remaps: vec![
                Remap {
                    kind: ItemKind::Volume,
                    from: "shop_appdata".into(),
                    to: "shop2_appdata".into(),
                },
                Remap {
                    kind: ItemKind::Image,
                    from: "shop-app:latest".into(),
                    to: "shop2-app:latest".into(),
                },
            ],
            not_in_backup: vec!["shop2_cache".into()],
        });
        let text = format_restore_preview(&p, false);
        assert!(text.contains("shop2 (backup made from shop)"), "{text}");
        assert!(
            text.contains("volume shop_appdata → shop2_appdata"),
            "{text}"
        );
        assert!(
            text.contains("image shop-app:latest → shop2-app:latest"),
            "{text}"
        );
        assert!(text.contains("Not in backup"), "{text}");
        assert!(text.contains("shop2_cache"), "{text}");
    }

    #[test]
    fn preview_names_a_full_backup_and_the_same_project_plainly() {
        let mut p = preview();
        p.compose = Some(ComposePreview {
            project: "shop".into(),
            backup_project: None,
            remaps: vec![],
            not_in_backup: vec![],
        });
        let text = format_restore_preview(&p, false);
        assert!(text.contains("shop (from a full backup)"), "{text}");
        assert!(
            !text.contains("Remapped") && !text.contains("Not in backup"),
            "{text}"
        );

        p.compose.as_mut().unwrap().backup_project = Some("shop".into());
        let text = format_restore_preview(&p, false);
        assert!(
            text.contains("Compose project") && !text.contains("made from"),
            "{text}"
        );
    }

    #[test]
    fn preview_without_compose_has_no_compose_rows() {
        assert!(!format_restore_preview(&preview(), false).contains("Compose project"));
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

    fn volume_prompt() -> VolumeOverwritePrompt {
        VolumeOverwritePrompt {
            target: "pgdata".into(),
            source: PathBuf::from("/backups/pgdata-20260926T141500Z.tar.bz2"),
            created_at: datetime!(2026-09-26 14:15:00 UTC),
        }
    }

    #[test]
    fn volume_overwrite_prompt_names_the_volume_the_file_and_the_backup_time() {
        assert_eq!(
            format_volume_overwrite_prompt(&volume_prompt()),
            "Volume pgdata will be emptied and refilled from \
             /backups/pgdata-20260926T141500Z.tar.bz2 (backed up 2026-09-26 14:15 UTC). \
             Continue? [y/N] "
        );
    }

    #[test]
    fn volume_overwrite_prompt_shows_the_backup_time_in_utc() {
        let mut prompt = volume_prompt();
        prompt.created_at = datetime!(2026-09-26 16:15:00 +02:00);
        assert!(
            format_volume_overwrite_prompt(&prompt).contains("(backed up 2026-09-26 14:15 UTC)")
        );
    }
}
