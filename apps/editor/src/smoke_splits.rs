use super::*;
use editor_services::ContentIdentity;

pub(super) async fn run_split_scenario(
    window: &AnyWindowHandle,
    workspace: &Entity<EditorWorkspace>,
    report: &mut SmokeReport,
    cx: &mut AsyncApp,
) -> Result<(), String> {
    run_startup_scenario(window, workspace, report, cx).await?;
    let large_fixture = cx.update(|cx| workspace.read(cx).session_layout().view_count() >= 100);
    if large_fixture {
        verify_fixture_visibility(workspace, report, cx)?;
        if !report.split_focus_first_budget_verified {
            measure_fixture_input(window, workspace, report, cx).await?;
        }
    } else {
        let (recovery, initial) = window.update(cx, |_, window, cx| {
            workspace.update(cx, |ws, cx| -> Result<_, String> {
                if ws.session_layout().panes().count() != 1 || ws.session_layout().view_count() != 1
                    || ws.is_lifecycle_busy() || ws.search.visible || ws.palette_visible || ws.goto_visible
                    || !matches!(ws.active_slot(), Some(TabSlot::Editable(ed)) if ed.visible_revision == Revision(0)
                        && ed.committed_byte_len() == 0 && !ed.is_dirty() && !ed.has_checkpoint
                        && !ed.has_undo_history() && !ed.has_pending_native_edit(cx) && ed.meta.path.is_none())
                {
                    return Err("splits native fixture requires one untouched scratch, not an existing user session".into());
                }
                ws.set_focus_only(false, window, cx);
                ws.capture_session_view_states(cx);
                ws.persist_session_layout(cx);
                Ok((ws.recovery.clone().ok_or("missing recovery worker")?, ws.session_layout().clone()))
            })
        }).map_err(|e| e.to_string())??;
        groups::wait_for_published_layout(&recovery, &initial, cx).await?;
        crate::group_ui::verify_pane_ui_native(window, workspace, cx).await?;
        cx.update(|cx| -> Result<(), String> {
            let ws = workspace.read(cx);
            if ws.session_layout().panes().count() < 2
                || ws.is_focus_only()
                || ws.is_compact_layout()
                || ws.is_lifecycle_busy()
                || ws.search.visible
                || ws.palette_visible
                || ws.measured_visible_pane_count() != ws.session_layout().panes().count()
            {
                return Err(
                    "native pane helper did not leave its promised settled visible tree".into(),
                );
            }
            ws.session_layout().validate().map_err(|e| e.to_string())
        })?;
        verify_protected_pane_close(window, workspace, cx).await?;
        report.split_native_ui_verified = true;
        verify_multiview_paged_native(window, workspace, cx).await?;
        report.multiview_paged_shared_source_verified = true;
    }
    let (recovery, expected) = window
        .update(cx, |_, _, cx| {
            workspace.update(cx, |ws, cx| -> Result<_, String> {
                ws.capture_session_view_states(cx);
                ws.persist_session_layout(cx);
                report.expected_group_drafts = ws
                    .all_view_ids()
                    .filter_map(|id| {
                        let target = ws.view_target(id)?;
                        let TabSlot::Editable(ed) = ws.view_slot(id)? else {
                            return None;
                        };
                        (ed.has_checkpoint || ed.visible_revision.0 > 0).then(|| {
                            (
                                target.document,
                                ContentIdentity::of_rope(ed.committed_rope()),
                                ed.visible_revision,
                            )
                        })
                    })
                    .collect();
                let expected = ws.session_layout().clone();
                report.expected_session_layout = Some(expected.clone());
                capture_workspace_metrics(ws, report);
                Ok((
                    ws.recovery.clone().ok_or("missing recovery worker")?,
                    expected,
                ))
            })
        })
        .map_err(|e| e.to_string())??;
    groups::wait_for_published_layout(&recovery, &expected, cx).await?;
    Ok(())
}

async fn verify_protected_pane_close(
    window: &AnyWindowHandle,
    workspace: &Entity<EditorWorkspace>,
    cx: &mut AsyncApp,
) -> Result<(), String> {
    wait_until(
        Duration::from_secs(10),
        cx,
        |cx| {
            cx.update(|cx| {
                let ws = workspace.read(cx);
                ws.all_view_ids().all(|id| {
                    matches!(ws.view_slot(id), Some(TabSlot::Editable(ed))
                        if ed.queued_revision == ed.visible_revision
                            && ed.durable_revision == ed.visible_revision
                            && !ed.has_pending_native_edit(cx))
                })
            })
        },
        "protected pane close fixture durability",
    )
    .await?;
    window
        .update(cx, |_, window, cx| {
            workspace.update(cx, |ws, cx| -> Result<(), String> {
                let protected = |target: &ViewTarget| {
                    matches!(ws.view_slot(target.view_id), Some(TabSlot::Editable(ed)) if ed.is_dirty())
                };
                let target = ws
                    .pane_active_target(ws.active_pane_id())
                    .filter(&protected)
                    .or_else(|| {
                        ws.all_view_ids()
                            .filter_map(|id| ws.view_target(id))
                            .find(protected)
                    })
                    .ok_or("pane close cancellation needs an existing protected Editable")?;
                ws.activate_view(target, window, cx);
                let Some(TabSlot::Editable(ed)) = ws.active_slot() else {
                    return Err("protected pane activation did not retain its Editable".into());
                };
                ed.focus(window, cx);
                Ok(())
            })
        })
        .map_err(|e| e.to_string())??;
    pump_frame(window, cx)?;
    let (targets, layout, before) = window
        .update(cx, |_, _, cx| {
            workspace.update(cx, |ws, cx| -> Result<_, String> {
                ws.capture_session_view_states(cx);
                let targets = ws
                    .pane_view_ids(ws.active_pane_id())
                    .ok_or("protected close pane disappeared")?
                    .iter()
                    .map(|id| {
                        ws.view_target(*id)
                            .ok_or("protected close target disappeared")
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if targets.len() == ws.session_layout().view_count() {
                    return Err(
                        "pane close cancellation needs an untouched other-pane document".into(),
                    );
                }
                let before = ws
                    .all_view_ids()
                    .map(|id| -> Result<_, String> {
                        let target = ws.view_target(id).ok_or("pane audit view disappeared")?;
                        let audit = groups::snapshot(ws, target)?;
                        let Some(TabSlot::Editable(ed)) = ws.view_slot(id) else {
                            return Err("pane audit lost an Editable".into());
                        };
                        Ok((
                            target,
                            audit,
                            ed.meta.clone(),
                            ed.editor().read(cx).presentation().is_readonly(),
                        ))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((targets, ws.session_layout().clone(), before))
            })
        })
        .map_err(|e| e.to_string())??;
    window
        .update(cx, |_, window, cx| {
            window.dispatch_action(Box::new(crate::actions::CloseActivePaneTabs), cx);
        })
        .map_err(|e| e.to_string())?;
    wait_until(
        Duration::from_secs(5),
        cx,
        |cx| cx.update(|cx| workspace.read(cx).pending_lifecycle_sequence().is_some()),
        "rendered protected Close Active Pane Tabs confirmation",
    )
    .await?;
    // Rendering must preserve the existing whole-session readonly barrier, even
    // though only the active pane's views participate in the close transaction.
    pump_frame(window, cx)?;
    cx.update(|cx| -> Result<(), String> {
        let ws = workspace.read(cx);
        if ws.session_layout() != &layout {
            return Err("protected pane close changed layout before confirmation".into());
        }
        for (target, expected, meta, _) in &before {
            let Some(TabSlot::Editable(ed)) = ws.view_slot(target.view_id) else {
                return Err("protected pane close replaced an editor before confirmation".into());
            };
            if !ed.editor().read(cx).presentation().is_readonly()
                || ed.has_pending_native_edit(cx)
                || &ed.meta != meta
                || groups::snapshot(ws, *target)? != *expected
            {
                return Err("rendered pane close barrier changed a buffer or lost readonly".into());
            }
        }
        Ok(())
    })?;
    groups::dispatch_dialog(window, cx, false)?;
    wait_until(
        Duration::from_secs(5),
        cx,
        |cx| cx.update(|cx| !workspace.read(cx).is_lifecycle_busy()),
        "protected pane close dialog cancellation",
    )
    .await?;
    pump_frame(window, cx)?;
    window
        .update(cx, |_, _, cx| {
            workspace.update(cx, |ws, cx| -> Result<(), String> {
                ws.capture_session_view_states(cx);
                if ws.session_layout() != &layout {
                    return Err("Cancel changed the complete pane layout or view state".into());
                }
                for (target, expected, meta, readonly) in &before {
                    let Some(TabSlot::Editable(ed)) = ws.view_slot(target.view_id) else {
                        return Err("Cancel lost a target or other-pane editor".into());
                    };
                    if ed.editor().read(cx).presentation().is_readonly() != *readonly
                        || ed.has_pending_native_edit(cx)
                        || &ed.meta != meta
                        || groups::snapshot(ws, *target)? != *expected
                    {
                        return Err(
                            "Cancel failed to restore all pane buffers and readonly states".into(),
                        );
                    }
                }
                Ok(())
            })
        })
        .map_err(|e| e.to_string())??;

    // Keep on the same rendered action proves the exact close targets through
    // consumer-visible removal, rather than inspecting private lifecycle state.
    let mut closed_layout = layout.clone();
    closed_layout
        .remove_views(&targets)
        .map_err(|e| e.to_string())?;
    window
        .update(cx, |_, window, cx| {
            window.dispatch_action(Box::new(crate::actions::CloseActivePaneTabs), cx);
        })
        .map_err(|e| e.to_string())?;
    wait_until(
        Duration::from_secs(5),
        cx,
        |cx| cx.update(|cx| workspace.read(cx).pending_lifecycle_sequence().is_some()),
        "second protected Close Active Pane Tabs confirmation",
    )
    .await?;
    groups::dispatch_dialog(window, cx, true)?;
    wait_until(
        Duration::from_secs(10),
        cx,
        |cx| cx.update(|cx| !workspace.read(cx).is_lifecycle_busy()),
        "protected pane Keep close acknowledgement",
    )
    .await?;
    pump_frame(window, cx)?;
    window
        .update(cx, |_, _, cx| {
            workspace.update(cx, |ws, cx| -> Result<(), String> {
                ws.capture_session_view_states(cx);
                if ws.session_layout() != &closed_layout {
                    return Err(
                        "Keep closed the wrong views, removed the pane, or altered other panes"
                            .into(),
                    );
                }
                for target in &targets {
                    if ws.documents.contains_key(&target.document.doc_id) {
                        return Err("acknowledged pane close retained a target editor".into());
                    }
                }
                for (target, expected, meta, readonly) in &before {
                    if targets.contains(target) {
                        continue;
                    }
                    let Some(TabSlot::Editable(ed)) = ws.view_slot(target.view_id) else {
                        return Err("pane Keep close lost an other-pane editor".into());
                    };
                    if ed.editor().read(cx).presentation().is_readonly() != *readonly
                        || ed.has_pending_native_edit(cx)
                        || &ed.meta != meta
                        || groups::snapshot(ws, *target)? != *expected
                    {
                        return Err(
                            "pane Keep close changed another pane's buffer or readonly state"
                                .into(),
                        );
                    }
                }
                Ok(())
            })
        })
        .map_err(|e| e.to_string())?
}

fn verify_fixture_visibility(
    workspace: &Entity<EditorWorkspace>,
    report: &mut SmokeReport,
    cx: &mut AsyncApp,
) -> Result<(), String> {
    cx.update(|cx| {
        let ws = workspace.read(cx);
        if ws.layout_dirty || ws.layout_write_count() != 0 {
            return Err("ordinary restored fixture issued a layout write before explicit smoke persistence".into());
        }
        let count = ws.session_layout().panes().count();
        if ![1, 2, 4, 8].contains(&count) || ws.is_compact_layout() || ws.is_focus_only()
            || ws.measured_visible_pane_count() != count || ws.visible_targets().count() != count
        {
            return Err("measurement requires all logical fixture panes actually visible in the ample window".into());
        }
        if ws.budget_config.max_hydrated_editors == 1 && count > 1 {
            if !matches!(ws.active_slot(), Some(TabSlot::Editable(_))) || ws.hydrated_editor_count() != 1
                || ws.visible_targets().filter(|target| Some(target.view_id) != ws.active_view_id())
                    .any(|target| !matches!(ws.view_slot(target.view_id), Some(TabSlot::OverBudgetDraft { .. })))
            {
                return Err("focus-first one-editor admission did not retain other visible drafts as OverBudget".into());
            }
            report.split_focus_first_budget_verified = true;
        } else if ws.hydrated_editor_count() != count || ws.visible_targets()
            .any(|target| !matches!(ws.view_slot(target.view_id), Some(TabSlot::Editable(_))))
        {
            return Err("fixture did not hydrate exactly one editable document per visible pane".into());
        }
        if ws.all_view_ids().filter(|id| !ws.visible_targets().any(|target| target.view_id == *id))
            .any(|id| !matches!(ws.view_slot(id), Some(TabSlot::Dehydrated(_))))
        {
            return Err("fixture eagerly loaded a hidden metadata tab".into());
        }
        Ok(())
    })
}

async fn measure_fixture_input(
    window: &AnyWindowHandle,
    workspace: &Entity<EditorWorkspace>,
    report: &mut SmokeReport,
    cx: &mut AsyncApp,
) -> Result<(), String> {
    let (target, editor, original, revision, tree, cursor) = window
        .update(cx, |_, window, cx| {
            workspace.update(cx, |ws, cx| -> Result<_, String> {
                let target = ws
                    .pane_active_target(ws.active_pane_id())
                    .ok_or("fixture selection missing")?;
                let Some(TabSlot::Editable(ed)) = ws.active_slot() else {
                    return Err("fixture focus is not Editable".into());
                };
                let editor = ed.editor().clone();
                let original = ed.committed_rope().clone();
                let cursor = editor.read(cx).cursor();
                let mid = original.floor_char_boundary(original.len() / 2);
                editor.update(cx, |state, cx| {
                    state.set_selected_range(mid..mid, cx);
                    state.focus(window, cx);
                });
                Ok((
                    target,
                    editor,
                    original,
                    ed.visible_revision,
                    ws.session_layout().tree.clone(),
                    cursor,
                ))
            })
        })
        .map_err(|e| e.to_string())??;
    pump_frame(window, cx)?;
    let mut samples = Vec::with_capacity(TYPING_BENCH_OPS);
    for index in 0..TYPING_BENCH_OPS {
        let started = Instant::now();
        window
            .update(cx, |_, window, cx| -> Result<(), String> {
                if index % 2 == 0 {
                    editor.update(cx, |state, cx| {
                        state.replace_text_in_range(
                            None,
                            if index % 4 == 0 { "a" } else { "中🙂" },
                            window,
                            cx,
                        );
                    });
                } else {
                    workspace.update(cx, |ws, cx| -> Result<(), String> {
                        if ws.pane_active_target(ws.active_pane_id()) != Some(target) {
                            return Err("input measurement lost focused document identity".into());
                        }
                        ws.active_editable_mut()
                            .ok_or("input measurement lost editor")?
                            .undo(window, cx);
                        Ok(())
                    })?;
                }
                Ok(())
            })
            .map_err(|e| e.to_string())??;
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    cx.update(|cx| -> Result<(), String> {
        let ws = workspace.read(cx);
        let Some(TabSlot::Editable(ed)) = ws.view_slot(target.view_id) else { return Err("measurement lost editor".into()) };
        if ed.editor().entity_id() != editor.entity_id() || ed.committed_rope() != &original
            || ed.visible_revision.0 != revision.0 + TYPING_BENCH_OPS as u64
            || ws.session_layout().tree != tree || ed.has_pending_native_edit(cx)
        {
            return Err("fixture input/Undo did not preserve original body/entity/pane tree or commit every sample".into());
        }
        Ok(())
    })?;
    window
        .update(cx, |_, _, cx| {
            editor.update(cx, |state, cx| state.set_selected_range(cursor..cursor, cx));
        })
        .map_err(|e| e.to_string())?;
    samples.sort_by(f64::total_cmp);
    report.typing_doc_bytes = original.len() as u64;
    report.typing_sample_count = samples.len();
    report.typing_submit_p50_ms = Some(percentile_sorted(&samples, 0.50));
    report.typing_submit_p95_ms = Some(percentile_sorted(&samples, 0.95));
    report.typing_submit_max_ms = samples.last().copied();
    report.split_fixture_input_verified = true;
    Ok(())
}

struct ScopedTempDir(PathBuf);

impl Drop for ScopedTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn verify_multiview_paged_native(
    window: &AnyWindowHandle,
    workspace: &Entity<EditorWorkspace>,
    cx: &mut AsyncApp,
) -> Result<(), String> {
    let raw_dir = cx.update(|cx| {
        workspace
            .read(cx)
            .cli
            .data_dir
            .join(format!("splits_multiview_{:032x}", DocumentId::new().0))
    });
    std::fs::create_dir_all(&raw_dir).map_err(|e| format!("create splits_multiview dir: {e}"))?;
    let dir = ScopedTempDir(std::fs::canonicalize(&raw_dir).unwrap_or(raw_dir));
    let paged_path = dir.0.join("multiview_paged.txt");
    {
        use std::io::Write as _;
        let file =
            std::fs::File::create(&paged_path).map_err(|e| format!("create paged file: {e}"))?;
        let mut writer = std::io::BufWriter::new(file);
        for i in 0..6_000usize {
            writeln!(writer, "line_{i:06}_0123456789abcdef_0123456789abcdef")
                .map_err(|e| format!("write paged file: {e}"))?;
        }
        writer
            .flush()
            .map_err(|e| format!("flush paged file: {e}"))?;
    }

    let result = verify_multiview_paged_inner(window, workspace, &paged_path, cx).await;
    let _ = cx.update(|cx| {
        workspace.update(cx, |ws, _| {
            ws.cli.editable_byte_limit = None;
        });
    });
    result?;
    drop(dir);
    cx.update(|cx| -> Result<(), String> {
        let ws = workspace.read(cx);
        if ws.cli.editable_byte_limit.is_some() {
            return Err(
                "expected ws.cli.editable_byte_limit to be None after temporary paged cleanup"
                    .into(),
            );
        }
        Ok(())
    })?;

    verify_persisted_multiview_paged_setup(window, workspace, cx).await
}

async fn verify_persisted_multiview_paged_setup(
    window: &AnyWindowHandle,
    workspace: &Entity<EditorWorkspace>,
    cx: &mut AsyncApp,
) -> Result<(), String> {
    let raw_data_dir = cx.update(|cx| workspace.read(cx).cli.data_dir.clone());
    std::fs::create_dir_all(&raw_data_dir)
        .map_err(|e| format!("create data_dir for persisted multiview paged: {e}"))?;
    let data_dir = std::fs::canonicalize(&raw_data_dir).unwrap_or(raw_data_dir);
    let raw_persisted_path = data_dir.join("persisted_multiview_paged.txt");
    {
        use std::io::Write as _;
        let file = std::fs::File::create(&raw_persisted_path)
            .map_err(|e| format!("create persisted paged file: {e}"))?;
        let mut writer = std::io::BufWriter::new(file);
        let long_line = vec![b'x'; 1_100_000];
        writer
            .write_all(&long_line)
            .map_err(|e| format!("write long line in persisted paged file: {e}"))?;
        writer
            .write_all(b"\n")
            .map_err(|e| format!("write newline after long line in persisted paged file: {e}"))?;
        for i in 0..200usize {
            writeln!(writer, "line_{i:04}_persisted")
                .map_err(|e| format!("write short line {i} in persisted paged file: {e}"))?;
        }
        writer
            .flush()
            .map_err(|e| format!("flush persisted paged file: {e}"))?;
    }
    let persisted_path = std::fs::canonicalize(&raw_persisted_path).unwrap_or(raw_persisted_path);

    window
        .update(cx, |_, window, cx| {
            workspace.update(cx, |ws, cx| {
                ws.open_path(persisted_path.clone(), window, cx);
            });
        })
        .map_err(|e| e.to_string())?;

    wait_until_with_frames(
        Duration::from_secs(10),
        window,
        cx,
        |cx| {
            cx.update(|cx| {
                let ws = workspace.read(cx);
                !ws.is_lifecycle_busy()
                    && ws.active_view_id().is_some_and(|id| {
                        ws.paged_view(id).is_some_and(|(src, vp)| {
                            src.meta.path.as_deref() == Some(persisted_path.as_path())
                                && vp.current_slice.is_some()
                                && !vp.is_loading
                        })
                    })
            })
        },
        "persisted multiview Paged initial viewport load",
    )
    .await?;

    let target_a = cx.update(|cx| -> Result<ViewTarget, String> {
        let ws = workspace.read(cx);
        let view_a = ws
            .active_view_id()
            .ok_or("missing active view_a for persisted multiview paged")?;
        let target_a = ws
            .view_target(view_a)
            .ok_or("missing ViewTarget for persisted multiview view_a")?;
        let (_, vp) = ws
            .paged_view(view_a)
            .ok_or("persisted multiview view_a missing paged_view")?;
        if vp.visible_byte_range().start != 0 {
            return Err(format!(
                "expected initial persisted paged offset 0, got {}",
                vp.visible_byte_range().start
            ));
        }
        if ws.session_layout().doc_view_count(target_a.document.doc_id) != 1 {
            return Err("expected doc_view_count == 1 after persisted paged open".into());
        }
        let sources = ws.paging_service.stats().sources;
        if sources != 1 {
            return Err(format!(
                "expected 1 paging source after persisted paged open, got {sources}"
            ));
        }
        Ok(target_a)
    })?;
    let doc_id = target_a.document.doc_id;

    focus_active_editor_or_workspace(window, workspace, cx)?;
    let (can_right, can_down) = cx.update(|cx| {
        let ws = workspace.read(cx);
        let root = &ws.session_layout().tree.root;
        let bounds = ws.group_ui.panes.root_bounds();
        let pane = ws.active_pane_id();
        let ratio = |id, fb| ws.pane_split_ratio(id).unwrap_or(fb);
        (
            crate::group_ui::pane_ui::can_split_pane_in_bounds(
                root,
                bounds,
                pane,
                editor_services::SplitDirection::Right,
                &ratio,
            )
            .is_ok(),
            crate::group_ui::pane_ui::can_split_pane_in_bounds(
                root,
                bounds,
                pane,
                editor_services::SplitDirection::Down,
                &ratio,
            )
            .is_ok(),
        )
    });
    if can_right {
        dispatch_window_action(window, cx, SplitAndCloneActiveViewRight)?;
    } else if can_down {
        dispatch_window_action(window, cx, crate::actions::SplitAndCloneActiveViewDown)?;
    } else {
        dispatch_window_action(window, cx, crate::actions::CloneActiveReadOnlyView)?;
    }

    wait_until_with_frames(
        Duration::from_secs(10),
        window,
        cx,
        |cx| {
            cx.update(|cx| {
                let ws = workspace.read(cx);
                let views = ws.session_layout().views_for_doc(doc_id);
                views.len() == 2
                    && views.iter().all(|vt| {
                        ws.paged_view(vt.view_id)
                            .is_some_and(|(_, vp)| vp.current_slice.is_some() && !vp.is_loading)
                    })
            })
        },
        "persisted multiview Paged split/clone secondary viewport hydration",
    )
    .await?;

    let target_b = cx.update(|cx| -> Result<ViewTarget, String> {
        let ws = workspace.read(cx);
        if ws.session_layout().doc_view_count(doc_id) != 2 {
            return Err(format!(
                "expected doc_view_count == 2 after persisted paged split/clone, got {}",
                ws.session_layout().doc_view_count(doc_id)
            ));
        }
        let target_b = ws
            .session_layout()
            .views_for_doc(doc_id)
            .into_iter()
            .find(|vt| vt.view_id != target_a.view_id)
            .ok_or("persisted paged split/clone did not create a distinct ViewId")?;
        if target_b.document != target_a.document {
            return Err("persisted paged split/clone did not share DocumentKey".into());
        }
        let sources = ws.paging_service.stats().sources;
        if sources != 1 {
            return Err(format!(
                "persisted paged split/clone allocated an extra PagedDocHandle: sources={sources}"
            ));
        }
        Ok(target_b)
    })?;
    pump_frame(window, cx)?;

    window
        .update(cx, |_, window, cx| {
            workspace.update(cx, |ws, cx| {
                ws.request_paged_viewport(
                    target_b,
                    ViewportAnchor::ByteOffset(524_288),
                    window,
                    cx,
                );
            });
        })
        .map_err(|e| e.to_string())?;

    wait_until_with_frames(
        Duration::from_secs(10),
        window,
        cx,
        |cx| {
            cx.update(|cx| {
                let ws = workspace.read(cx);
                ws.paged_view(target_b.view_id).is_some_and(|(_, vp)| {
                    vp.current_slice.is_some()
                        && !vp.is_loading
                        && vp.visible_byte_range().start >= 524_288
                })
            })
        },
        "persisted multiview Paged target_b navigation to >= 524_288",
    )
    .await?;

    cx.update(|cx| -> Result<(), String> {
        let ws = workspace.read(cx);
        if ws.session_layout().doc_view_count(doc_id) != 2 {
            return Err("persisted multiview Paged did not preserve doc_view_count == 2".into());
        }
        if ws.paging_service.stats().sources != 1 {
            return Err(format!(
                "expected 1 shared paging source before split shutdown, got {}",
                ws.paging_service.stats().sources
            ));
        }
        let (_, vp_a) = ws
            .paged_view(target_a.view_id)
            .ok_or("persisted multiview target_a missing after target_b navigation")?;
        if vp_a.current_slice.is_none() || vp_a.is_loading || vp_a.visible_byte_range().start != 0 {
            return Err(format!(
                "persisted multiview target_a mutated to {:?}",
                vp_a.visible_byte_range()
            ));
        }
        let (_, vp_b) = ws
            .paged_view(target_b.view_id)
            .ok_or("persisted multiview target_b missing after navigation")?;
        if vp_b.current_slice.is_none()
            || vp_b.is_loading
            || vp_b.visible_byte_range().start < 524_288
        {
            return Err(format!(
                "persisted multiview target_b did not retain >= 524_288 range: {:?}",
                vp_b.visible_byte_range()
            ));
        }
        ws.session_layout().validate().map_err(|e| e.to_string())?;
        Ok(())
    })?;

    Ok(())
}

async fn verify_multiview_paged_inner(
    window: &AnyWindowHandle,
    workspace: &Entity<EditorWorkspace>,
    paged_path: &Path,
    cx: &mut AsyncApp,
) -> Result<(), String> {
    let (baseline_sources, baseline_panes) = cx.update(|cx| {
        let ws = workspace.read(cx);
        (
            ws.paging_service.stats().sources,
            ws.session_layout().panes().count(),
        )
    });
    if baseline_sources != 0 {
        return Err(format!(
            "expected 0 active paging sources before multiview test, got {baseline_sources}"
        ));
    }

    // 1) Open ~300 KiB file with temporary 256 KiB limit -> lands in Paged mode at byte 0.
    window
        .update(cx, |_, window, cx| {
            workspace.update(cx, |ws, cx| {
                ws.cli.editable_byte_limit = Some(256 * 1024);
                ws.open_path(paged_path.to_path_buf(), window, cx);
            });
        })
        .map_err(|e| e.to_string())?;

    wait_until_with_frames(
        Duration::from_secs(10),
        window,
        cx,
        |cx| {
            cx.update(|cx| {
                let ws = workspace.read(cx);
                !ws.is_lifecycle_busy()
                    && ws.active_view_id().is_some_and(|id| {
                        ws.paged_view(id).is_some_and(|(src, vp)| {
                            src.meta.path.as_deref() == Some(paged_path)
                                && vp.current_slice.is_some()
                                && !vp.is_loading
                        })
                    })
            })
        },
        "multiview Paged initial viewport load",
    )
    .await?;

    let target_1 = cx.update(|cx| -> Result<ViewTarget, String> {
        let ws = workspace.read(cx);
        let view_1 = ws.active_view_id().ok_or("missing active paged view_1")?;
        let target_1 = ws
            .view_target(view_1)
            .ok_or("missing ViewTarget for view_1")?;
        let (_, vp) = ws.paged_view(view_1).ok_or("view_1 missing paged_view")?;
        if vp.visible_byte_range().start != 0 {
            return Err(format!(
                "expected initial paged offset 0, got {}",
                vp.visible_byte_range().start
            ));
        }
        if ws.session_layout().doc_view_count(target_1.document.doc_id) != 1 {
            return Err("expected doc_view_count == 1 after initial paged open".into());
        }
        let sources = ws.paging_service.stats().sources;
        if sources != 1 {
            return Err(format!(
                "expected 1 paging source after open, got {sources}"
            ));
        }
        Ok(target_1)
    })?;
    let doc_id = target_1.document.doc_id;
    focus_active_editor_or_workspace(window, workspace, cx)?;

    // 2) SplitAndCloneActiveViewRight -> creates view_2 sharing exact same doc_id & PagedDocHandle.
    dispatch_window_action(window, cx, SplitAndCloneActiveViewRight)?;
    wait_until_with_frames(
        Duration::from_secs(10),
        window,
        cx,
        |cx| {
            cx.update(|cx| {
                let ws = workspace.read(cx);
                let views = ws.session_layout().views_for_doc(doc_id);
                views.len() == 2
                    && views.iter().all(|vt| {
                        ws.paged_view(vt.view_id)
                            .is_some_and(|(_, vp)| vp.current_slice.is_some() && !vp.is_loading)
                    })
            })
        },
        "SplitAndCloneActiveViewRight secondary viewport hydration",
    )
    .await?;

    let target_2 = cx.update(|cx| -> Result<ViewTarget, String> {
        let ws = workspace.read(cx);
        let views = ws.session_layout().views_for_doc(doc_id);
        let target_2 = views
            .into_iter()
            .find(|vt| vt.view_id != target_1.view_id)
            .ok_or("SplitAndCloneActiveViewRight did not create a distinct ViewId")?;
        if target_2.document != target_1.document {
            return Err("SplitAndCloneActiveViewRight did not share DocumentKey".into());
        }
        if ws.session_layout().pane_for_view(target_2.view_id)
            == ws.session_layout().pane_for_view(target_1.view_id)
        {
            return Err("SplitAndCloneActiveViewRight did not place view_2 in a new pane".into());
        }
        if ws.active_view_id() != Some(target_2.view_id) {
            return Err("SplitAndCloneActiveViewRight did not focus new view_2".into());
        }
        let sources = ws.paging_service.stats().sources;
        if sources != 1 {
            return Err(format!(
                "SplitAndCloneActiveViewRight allocated an extra PagedDocHandle: sources={sources}"
            ));
        }
        Ok(target_2)
    })?;
    pump_frame(window, cx)?;

    // 3) Navigate view_2 to byte offset >= 200_000 while view_1 remains independently at 0.
    window
        .update(cx, |_, window, cx| {
            workspace.update(cx, |ws, cx| {
                ws.request_paged_viewport(
                    target_2,
                    ViewportAnchor::ByteOffset(200_000),
                    window,
                    cx,
                );
            });
        })
        .map_err(|e| e.to_string())?;

    wait_until_with_frames(
        Duration::from_secs(10),
        window,
        cx,
        |cx| {
            cx.update(|cx| {
                let ws = workspace.read(cx);
                ws.paged_view(target_2.view_id).is_some_and(|(_, vp)| {
                    !vp.is_loading && vp.visible_byte_range().start >= 200_000
                })
            })
        },
        "independent navigation of view_2 to byte offset >= 200_000",
    )
    .await?;

    cx.update(|cx| -> Result<(), String> {
        let ws = workspace.read(cx);
        let (_, vp1) = ws
            .paged_view(target_1.view_id)
            .ok_or("view_1 missing after view_2 navigation")?;
        if vp1.visible_byte_range().start != 0 {
            return Err(format!(
                "navigating view_2 mutated view_1 viewport offset to {}",
                vp1.visible_byte_range().start
            ));
        }
        Ok(())
    })?;
    focus_active_editor_or_workspace(window, workspace, cx)?;

    // 4) Close secondary view (view_2) via CloseActiveTab: immediate fast-path, no dialog, view_1 intact.
    dispatch_window_action(window, cx, CloseActiveTab)?;
    pump_frame(window, cx)?;
    cx.update(|cx| -> Result<(), String> {
        let ws = workspace.read(cx);
        if ws.pending_lifecycle_sequence().is_some() || ws.is_lifecycle_busy() {
            return Err(
                "closing secondary Paged view unexpectedly entered lifecycle/dialog".into(),
            );
        }
        if ws.session_layout().doc_view_count(doc_id) != 1 {
            return Err("closing secondary Paged view did not leave doc_view_count == 1".into());
        }
        if ws.session_layout().views.contains_key(&target_2.view_id)
            || ws.paged_viewports.contains_key(&target_2.view_id)
        {
            return Err("secondary Paged view or viewport survived CloseActiveTab".into());
        }
        let (_, vp1) = ws
            .paged_view(target_1.view_id)
            .ok_or("primary view_1 lost after secondary close")?;
        if vp1.visible_byte_range().start != 0 {
            return Err("primary view_1 offset changed after secondary close".into());
        }
        if ws.paging_service.stats().sources != 1 {
            return Err("closing secondary view dropped shared PagedDocHandle".into());
        }
        Ok(())
    })?;

    if cx.update(|cx| {
        let ws = workspace.read(cx);
        ws.pane_view_ids(ws.active_pane_id()) == Some(&[][..])
            && ws.session_layout().panes().count() > baseline_panes
    }) {
        focus_active_editor_or_workspace(window, workspace, cx)?;
        dispatch_window_action(window, cx, CloseActivePane)?;
    }
    pump_frame(window, cx)?;

    // 5) Re-split/clone view_1 -> view_3 sharing doc_id.
    window
        .update(cx, |_, window, cx| {
            workspace.update(cx, |ws, cx| {
                if ws.active_view_id() != Some(target_1.view_id) {
                    ws.activate_view(target_1, window, cx);
                }
            });
        })
        .map_err(|e| e.to_string())?;
    focus_active_editor_or_workspace(window, workspace, cx)?;

    dispatch_window_action(window, cx, SplitAndCloneActiveViewRight)?;
    wait_until_with_frames(
        Duration::from_secs(10),
        window,
        cx,
        |cx| {
            cx.update(|cx| {
                let ws = workspace.read(cx);
                let views = ws.session_layout().views_for_doc(doc_id);
                views.len() == 2
                    && views.iter().all(|vt| {
                        ws.paged_view(vt.view_id)
                            .is_some_and(|(_, vp)| vp.current_slice.is_some() && !vp.is_loading)
                    })
            })
        },
        "second SplitAndCloneActiveViewRight viewport hydration",
    )
    .await?;

    let target_3 = cx.update(|cx| -> Result<ViewTarget, String> {
        let ws = workspace.read(cx);
        ws.session_layout()
            .views_for_doc(doc_id)
            .into_iter()
            .find(|vt| vt.view_id != target_1.view_id)
            .ok_or("second SplitAndCloneActiveViewRight missing view_3".into())
    })?;
    focus_active_editor_or_workspace(window, workspace, cx)?;

    // 6) Truncate backing file on disk and ReloadActiveDocument -> ADR 009 shrink-to-Editable collapse.
    std::fs::write(paged_path, "small_after_reload\n")
        .map_err(|e| format!("truncate paged file for reload: {e}"))?;
    cx.update(|cx| {
        workspace.update(cx, |ws, _| {
            ws.cli.editable_byte_limit = None;
        });
    });

    dispatch_window_action(window, cx, ReloadActiveDocument)?;
    wait_until_with_frames(
        Duration::from_secs(10),
        window,
        cx,
        |cx| {
            cx.update(|cx| {
                let ws = workspace.read(cx);
                !ws.is_lifecycle_busy()
                    && matches!(
                        ws.active_slot(),
                        Some(TabSlot::Editable(ed))
                            if ed.doc_id != doc_id && ed.meta.path.as_deref() == Some(paged_path)
                    )
            })
        },
        "ReloadActiveDocument shrink-to-Editable collapse",
    )
    .await?;

    // 7) Verify ADR 009 shrink-to-Editable invariants.
    let (new_target, new_doc_id) = cx.update(|cx| -> Result<(ViewTarget, DocumentId), String> {
        let ws = workspace.read(cx);
        let active_id = ws
            .active_view_id()
            .ok_or("missing active view after shrink-to-Editable reload")?;
        let new_target = ws
            .view_target(active_id)
            .ok_or("missing ViewTarget after reload")?;
        let Some(TabSlot::Editable(ed)) = ws.active_slot() else {
            return Err("reloaded small file did not land in TabSlot::Editable".into());
        };
        let new_doc_id = ed.doc_id;
        if new_doc_id == doc_id {
            return Err("ReloadActiveDocument kept old DocumentId instead of fresh one".into());
        }
        if ws.session_layout().doc_view_count(new_doc_id) != 1 {
            return Err(format!(
                "ADR 009 shrink-to-Editable left {} views instead of 1",
                ws.session_layout().doc_view_count(new_doc_id)
            ));
        }
        if ws.session_layout().doc_view_count(doc_id) != 0 || ws.documents.contains_key(&doc_id) {
            return Err("old Paged document remained after shrink-to-Editable reload".into());
        }
        if ed.committed_rope().to_string() != "small_after_reload\n" {
            return Err(format!(
                "reloaded Editable content mismatch: {:?}",
                ed.committed_rope().to_string()
            ));
        }
        let collapsed_secondary = if new_target.view_id == target_3.view_id {
            target_1.view_id
        } else {
            target_3.view_id
        };
        if ws.session_layout().views.contains_key(&collapsed_secondary)
            || ws.paged_viewports.contains_key(&target_1.view_id)
            || ws.paged_viewports.contains_key(&target_3.view_id)
        {
            return Err("secondary Paged view or viewport survived ADR 009 collapse".into());
        }
        if ws.paging_service.stats().sources != 0 {
            return Err(
                "old PagedDocHandle was not dropped after shrink-to-Editable reload".into(),
            );
        }
        Ok((new_target, new_doc_id))
    })?;

    // 8) Cleanly close the temporary reloaded Editable tab and extra empty split pane.
    focus_active_editor_or_workspace(window, workspace, cx)?;
    dispatch_window_action(window, cx, CloseActiveTab)?;
    wait_until_with_frames(
        Duration::from_secs(10),
        window,
        cx,
        |cx| {
            cx.update(|cx| {
                let ws = workspace.read(cx);
                !ws.is_lifecycle_busy()
                    && !ws.session_layout().views.contains_key(&new_target.view_id)
                    && !ws.documents.contains_key(&new_doc_id)
            })
        },
        "closing temporary reloaded Editable tab",
    )
    .await?;

    if cx.update(|cx| {
        let ws = workspace.read(cx);
        ws.session_layout().panes().count() > baseline_panes
            && ws.pane_view_ids(ws.active_pane_id()) == Some(&[][..])
    }) {
        focus_active_editor_or_workspace(window, workspace, cx)?;
        dispatch_window_action(window, cx, CloseActivePane)?;
    }
    wait_until_with_frames(
        Duration::from_secs(10),
        window,
        cx,
        |cx| {
            cx.update(|cx| {
                let ws = workspace.read(cx);
                !ws.is_lifecycle_busy() && !ws.layout_dirty
            })
        },
        "settling SessionLayout after multiview cleanup",
    )
    .await?;

    let (recovery, settled_layout) = cx.update(|cx| -> Result<_, String> {
        let ws = workspace.read(cx);
        if ws.session_layout().panes().count() != baseline_panes {
            return Err(format!(
                "multiview cleanup left {} panes instead of {baseline_panes}",
                ws.session_layout().panes().count()
            ));
        }
        ws.session_layout().validate().map_err(|e| e.to_string())?;
        Ok((
            ws.recovery.clone().ok_or("missing recovery worker")?,
            ws.session_layout().clone(),
        ))
    })?;
    groups::wait_for_published_layout(&recovery, &settled_layout, cx).await?;

    Ok(())
}
