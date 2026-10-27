use super::{
    agenda::*,
    agenda_projection::{Projection, date},
    agenda_store::model::*,
};
use serde_json::json;
fn record(id: &str, kind: ItemKind, completed: bool, schedule: Schedule) -> Record {
    Record {
        id: id.into(),
        kind,
        source_id: "local".into(),
        source_uid: None,
        revision: 1,
        title: id.into(),
        notes: String::new(),
        links: vec![],
        schedule,
        created_unix_ms: 1,
        modified_unix_ms: 1,
        capabilities: Capabilities {
            write: true,
            delete: true,
            complete: kind == ItemKind::Task,
        },
        event: (kind == ItemKind::Event).then(EventDetails::default),
        task: (kind == ItemKind::Task).then(|| TaskDetails {
            completed,
            ..TaskDetails::default()
        }),
    }
}
fn request(day: Option<&str>) -> ListRequest {
    ListRequest {
        day: day.map(str::to_owned),
        ..ListRequest::default()
    }
}
#[test]
fn all_endpoint_cases_types_and_completion_states_share_list_and_marker_membership() {
    let p = Projection {
        today: date("2026-10-27").unwrap(),
        zone: chrono_tz::UTC,
    };
    for timed in [false, true] {
        for (s, e, expected) in [
            (Some(26), Some(28), vec![26, 27, 28]),
            (None, Some(28), vec![27, 28]),
            (Some(26), None, vec![26, 27]),
            (None, Some(26), vec![]),
            (Some(28), None, vec![]),
            (None, None, vec![]),
        ] {
            let schedule: Schedule = if timed {
                let ms = |n| {
                    date(&format!("2026-10-{n}"))
                        .unwrap()
                        .and_hms_opt(12, 0, 0)
                        .unwrap()
                        .and_utc()
                        .timestamp_millis()
                };
                Schedule {
                    mode: ScheduleMode::Timed,
                    start_unix_ms: s.map(ms),
                    end_unix_ms: e.map(ms),
                    timezone: Some("UTC".into()),
                    ..Schedule::default()
                }
            } else {
                Schedule {
                    start_date: s.map(|n| format!("2026-10-{n}")),
                    end_date: e.map(|n| format!("2026-10-{n}")),
                    ..Schedule::default()
                }
            };
            let records = [
                record("a", ItemKind::Event, false, schedule.clone()),
                record("b", ItemKind::Event, true, schedule.clone()),
                record("c", ItemKind::Task, false, schedule.clone()),
                record("d", ItemKind::Task, true, schedule),
            ];
            let catalog = Catalog::new(records.to_vec(), &p);
            let markers = markers(
                &catalog,
                1,
                &p,
                "UTC",
                MarkersRequest {
                    from_date: "2026-10-25".into(),
                    to_date: "2026-10-29".into(),
                },
            )
            .unwrap();
            for marker in markers.days {
                let n: i32 = marker.date[8..].parse().unwrap();
                let list = list(
                    &catalog,
                    1,
                    "imports",
                    &p,
                    "UTC",
                    request(Some(&marker.date)),
                )
                .unwrap();
                assert_eq!(
                    list.items.len(),
                    if expected.contains(&n) { 4 } else { 0 },
                    "timed={timed} start={s:?} end={e:?} day={n}"
                );
                assert_eq!(marker.event, !list.items.is_empty());
                assert_eq!(marker.task, marker.event);
            }
            assert_eq!(
                list(&catalog, 1, "imports", &p, "UTC", request(None))
                    .unwrap()
                    .items
                    .len(),
                4,
                "list-only records retained"
            );
        }
    }
}
#[test]
fn ordering_search_windows_and_every_cursor_fence() {
    let p = Projection {
        today: date("2026-10-27").unwrap(),
        zone: chrono_tz::UTC,
    };
    let civil = |s: &str, e: &str| Schedule {
        start_date: Some(s.into()),
        end_date: Some(e.into()),
        ..Schedule::default()
    };
    let mut records = vec![
        record("undated", ItemKind::Task, true, Schedule::default()),
        record(
            "past-old",
            ItemKind::Event,
            false,
            civil("2026-10-20", "2026-10-21"),
        ),
        record(
            "past-new",
            ItemKind::Task,
            false,
            civil("2026-10-25", "2026-10-26"),
        ),
        record(
            "future",
            ItemKind::Task,
            true,
            civil("2026-10-28", "2026-10-29"),
        ),
        record(
            "today-z",
            ItemKind::Task,
            true,
            civil("2026-10-27", "2026-10-27"),
        ),
        record(
            "today-a",
            ItemKind::Task,
            false,
            civil("2026-10-27", "2026-10-27"),
        ),
    ];
    records[0].notes = "unique needle".into();
    let catalog = Catalog::new(records.clone(), &p);
    let all = list(&catalog, 1, "imports", &p, "UTC", request(None)).unwrap();
    assert_eq!(
        all.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
        vec![
            "today-a", "today-z", "future", "past-new", "past-old", "undated"
        ]
    );
    let first = list(
        &catalog,
        1,
        "imports",
        &p,
        "UTC",
        ListRequest {
            limit: Some(1),
            ..request(None)
        },
    )
    .unwrap();
    let cursor = first.next_cursor.unwrap();
    let next = || ListRequest {
        cursor: Some(cursor.clone()),
        limit: Some(1),
        ..request(None)
    };
    assert_eq!(
        list(&catalog, 1, "imports", &p, "UTC", next())
            .unwrap()
            .items[0]
            .id,
        "today-z"
    );
    assert!(list(&catalog, 2, "imports", &p, "UTC", next()).is_err());
    assert!(list(&catalog, 1, "changed-imports", &p, "UTC", next()).is_err());
    assert!(list(&catalog, 1, "imports", &p, "Europe/Berlin", next()).is_err());
    let tomorrow = Projection {
        today: p.today.succ_opt().unwrap(),
        zone: p.zone,
    };
    assert!(list(&catalog, 1, "imports", &tomorrow, "UTC", next()).is_err());
    assert!(
        list(
            &catalog,
            1,
            "imports",
            &p,
            "UTC",
            ListRequest {
                query: "needle".into(),
                ..next()
            }
        )
        .is_err()
    );
    assert!(
        list(
            &catalog,
            1,
            "imports",
            &p,
            "UTC",
            ListRequest {
                day: Some("2026-10-27".into()),
                ..next()
            }
        )
        .is_err()
    );
    let found = list(
        &catalog,
        1,
        "imports",
        &p,
        "UTC",
        ListRequest {
            query: "UNIQUE needle".into(),
            limit: Some(1),
            ..request(None)
        },
    )
    .unwrap();
    assert_eq!(found.items[0].id, "undated");
    for r in &mut records {
        if let Some(t) = &mut r.task {
            t.completed = !t.completed;
        }
    }
    let toggled = list(
        &Catalog::new(records, &p),
        2,
        "imports",
        &p,
        "UTC",
        request(None),
    )
    .unwrap();
    assert_eq!(
        all.items.iter().map(|i| &i.id).collect::<Vec<_>>(),
        toggled.items.iter().map(|i| &i.id).collect::<Vec<_>>()
    );
    assert!(
        list(
            &catalog,
            1,
            "imports",
            &p,
            "UTC",
            ListRequest {
                limit: Some(101),
                ..request(None)
            }
        )
        .is_err()
    );
    assert!(
        markers(
            &catalog,
            1,
            &p,
            "UTC",
            serde_json::from_value(json!({"from_date":"2026-01-01","to_date":"2027-01-06"}))
                .unwrap()
        )
        .is_err()
    );
}
#[test]
fn list_summaries_are_lazy_and_byte_bounded_without_dropping_records() {
    let p = Projection {
        today: date("2026-10-27").unwrap(),
        zone: chrono_tz::UTC,
    };
    let records = (0..100)
        .map(|n| {
            let mut item = record(
                &format!("id-{n:03}"),
                ItemKind::Event,
                false,
                Schedule::default(),
            );
            item.title = "\u{1}".repeat(4096);
            item.notes = "private large notes".repeat(1000);
            item.event.as_mut().unwrap().location = "\u{1}".repeat(8192);
            item.event.as_mut().unwrap().people = vec!["not in summary".into()];
            item
        })
        .collect();
    let catalog = Catalog::new(records, &p);
    let mut cursor = None;
    let mut ids = Vec::new();
    let mut pages = 0;
    loop {
        let page = list(
            &catalog,
            1,
            "imports",
            &p,
            "UTC",
            ListRequest {
                cursor,
                ..request(None)
            },
        )
        .unwrap();
        assert!(page.items.len() < 100);
        let wire = serde_json::to_value(&page).unwrap();
        assert!(serde_json::to_vec(&wire).unwrap().len() < 2 * 1024 * 1024);
        assert_eq!(wire["items"][0]["summary"], true);
        assert!(wire["items"][0].get("notes").is_none());
        assert!(wire["items"][0].get("links").is_none());
        assert!(wire["items"][0]["event"].get("people").is_none());
        ids.extend(page.items.into_iter().map(|i| i.id));
        pages += 1;
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert!(pages > 1);
    assert_eq!(ids.len(), 100);
    assert_eq!(
        ids.into_iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        100
    );
    assert!(!catalog.get("id-050").unwrap().notes.is_empty());
}
#[test]
fn timed_nearest_order_retains_instant_precision_and_completion_does_not_matter() {
    let p = Projection {
        today: date("2026-10-27").unwrap(),
        zone: chrono_tz::UTC,
    };
    let timed = |start: &str, end: &str| Schedule {
        mode: ScheduleMode::Timed,
        start_unix_ms: Some(
            chrono::DateTime::parse_from_rfc3339(start)
                .unwrap()
                .timestamp_millis(),
        ),
        end_unix_ms: Some(
            chrono::DateTime::parse_from_rfc3339(end)
                .unwrap()
                .timestamp_millis(),
        ),
        timezone: Some("UTC".into()),
        ..Schedule::default()
    };
    let mut records = vec![
        record(
            "a-late",
            ItemKind::Task,
            true,
            timed("2026-10-28T23:00:00Z", "2026-10-28T23:30:00Z"),
        ),
        record(
            "z-early",
            ItemKind::Task,
            false,
            timed("2026-10-28T10:00:00Z", "2026-10-28T11:00:00Z"),
        ),
        record(
            "a-past-old",
            ItemKind::Task,
            false,
            timed("2026-10-26T10:00:00Z", "2026-10-26T11:00:00Z"),
        ),
        record(
            "z-past-new",
            ItemKind::Task,
            true,
            timed("2026-10-26T22:00:00Z", "2026-10-26T23:00:00Z"),
        ),
    ];
    for _ in 0..2 {
        let catalog = Catalog::new(records.clone(), &p);
        let ordered = list(&catalog, 1, "imports", &p, "UTC", request(None)).unwrap();
        assert_eq!(
            ordered
                .items
                .iter()
                .map(|i| i.id.as_str())
                .collect::<Vec<_>>(),
            vec!["z-early", "a-late", "z-past-new", "a-past-old"]
        );
        for record in &mut records {
            let task = record.task.as_mut().unwrap();
            task.completed = !task.completed;
        }
    }
}
#[test]
fn leap_year_civil_dates_and_timezone_change_reproject_without_writes() {
    let s = Schedule {
        start_date: Some("2024-02-28".into()),
        end_date: Some("2024-03-01".into()),
        ..Schedule::default()
    };
    let p = Projection {
        today: date("2024-02-29").unwrap(),
        zone: chrono_tz::Pacific::Auckland,
    };
    assert!(p.includes(&s, p.today));
    let ms = chrono::DateTime::parse_from_rfc3339("2026-12-31T23:30:00Z")
        .unwrap()
        .timestamp_millis();
    let timed = Schedule {
        mode: ScheduleMode::Timed,
        start_unix_ms: Some(ms),
        end_unix_ms: Some(ms + 30 * 60 * 1000),
        timezone: Some("UTC".into()),
        ..Schedule::default()
    };
    let utc = Projection {
        today: date("2026-12-31").unwrap(),
        zone: chrono_tz::UTC,
    };
    let tokyo = Projection {
        today: date("2027-01-01").unwrap(),
        zone: chrono_tz::Asia::Tokyo,
    };
    assert!(utc.includes(&timed, utc.today));
    assert!(!utc.includes(&timed, tokyo.today));
    assert!(tokyo.includes(&timed, tokyo.today));
    assert!(!tokyo.includes(&s, tokyo.today));
    assert!(date("2026-02-29").is_err());
    assert!(date("2026-2-09").is_err());
}
