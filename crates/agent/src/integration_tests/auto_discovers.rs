use std::time::Duration;

use crate::{
    controllers::capture::DiscoverChange,
    integration_tests::harness::{draft_catalog, InjectBuildError, TestHarness},
    publications,
};
use proto_flow::capture::response::{discovered::Binding, Discovered};
use serde_json::json;

// Testing auto-discovers is a bit tricky because we don't really have the
// ability to "fast-forward" time. We have to just set the interval to something
// very low and wait until it's due. We can't use a 0 interval, though, because
// then the auto-discover will run literally every time the controller runs,
// even when it's just supposed to activate. So we resign ourselves to `sleep`,
// and just try to make it as fast as possible.
const AUTO_DISCOVER_WAIT: Duration = Duration::from_millis(20);
const AUTO_DISCOVER_INTERVAL: &str = "15ms";

#[tokio::test]
#[serial_test::serial]
async fn test_auto_discovers_add_new_bindings() {
    let mut harness = TestHarness::init("test_auto_discovers_new").await;

    let user_id = harness.setup_tenant("marmots").await;

    let init_draft = draft_catalog(json!({
        "captures": {
            "marmots/capture": {
                "autoDiscover": {
                    "addNewBindings": true,
                    "evolveIncompatibleCollections": true,
                },
                "shards": {
                    "logLevel": "debug"
                },
                "interval": "42s",
                "endpoint": {
                    "connector": {
                        "image": "source/test:test",
                        "config": { "squeak": "squeak" }
                    },
                },
                "bindings": [ ]
            },
            "marmots/no-auto-discover": {
                "endpoint": {
                    "connector": {
                        "image": "source/test:test",
                        "config": { "squeak": "squeak" }
                    },
                },
                "bindings": [ ]
            }
        },
        "materializations": {
            "marmots/materialize": {
                "sourceCapture": "marmots/capture",
                "endpoint": {
                    "connector": {
                        "image": "materialize/test:test",
                        "config": { "squeak": "squeak squeak" }
                    }
                },
                "bindings": []
            }
        }
    }));

    let result = harness
        .user_publication(user_id, "initial publication", init_draft)
        .await;
    assert!(result.status.is_success());
    assert_eq!(3, result.live_specs.len());

    harness.run_pending_controllers(Some(3)).await;

    // Assert that we've initialized auto-discover state appropriately.
    let capture_state = harness.get_controller_state("marmots/capture").await;
    assert!(capture_state
        .current_status
        .unwrap_capture()
        .auto_discover
        .is_some());
    assert!(capture_state.next_run.is_some());

    let no_disco_capture_state = harness
        .get_controller_state("marmots/no-auto-discover")
        .await;
    assert!(no_disco_capture_state
        .current_status
        .unwrap_capture()
        .auto_discover
        .is_none());
    assert!(no_disco_capture_state.next_run.is_none());

    harness
        .set_auto_discover_interval("marmots/capture", AUTO_DISCOVER_INTERVAL)
        .await;
    let discovered = Discovered {
        bindings: vec![
            Binding {
                recommended_name: "grass".to_string(),
                resource_config_json: r#"{"id": "grass", "extra": "grass" }"#.to_string(),
                document_schema_json: document_schema(2).to_string(),
                key: vec!["/id".to_string()],
                disable: false,
                resource_path: Vec::new(),
            },
            Binding {
                recommended_name: "moss".to_string(),
                resource_config_json: r#"{"id": "moss", "extra": "stuff" }"#.to_string(),
                document_schema_json: document_schema(1).to_string(),
                key: vec!["/id".to_string()],
                disable: true,
                resource_path: Vec::new(),
            },
        ],
    };

    harness
        .discover_handler
        .connectors
        .mock_discover(Box::new(Ok(discovered)));
    tokio::time::sleep(AUTO_DISCOVER_WAIT).await;
    harness.run_pending_controller("marmots/capture").await;

    let capture_state = harness.get_controller_state("marmots/capture").await;
    let model = capture_state
        .live_spec
        .as_ref()
        .unwrap()
        .as_capture()
        .unwrap();
    // Expect to see the new bindings added
    insta::assert_json_snapshot!(model.bindings, @r###"
    [
      {
        "resource": {
          "$serde_json::private::RawValue": "{\"extra\":\"grass\",\"id\":\"grass\"}"
        },
        "target": "marmots/grass"
      },
      {
        "resource": {
          "$serde_json::private::RawValue": "{\"extra\":\"stuff\",\"id\":\"moss\"}"
        },
        "disable": true,
        "target": "marmots/moss"
      }
    ]
    "###);

    let status = capture_state.current_status.unwrap_capture();
    let auto_discover = status
        .auto_discover
        .as_ref()
        .unwrap()
        .last_success
        .as_ref()
        .unwrap();
    insta::assert_json_snapshot!(auto_discover, {
        ".ts" => "[ts]",
    }, @r###"
    {
      "ts": "[ts]",
      "added": [
        {
          "resource_path": [
            "grass"
          ],
          "target": "marmots/grass",
          "disable": false
        },
        {
          "resource_path": [
            "moss"
          ],
          "target": "marmots/moss",
          "disable": true
        }
      ],
      "publish_result": {
        "type": "success"
      }
    }
    "###);
    let last_success_time = auto_discover.ts;

    // Subsequent discover with the same bindings should result in a no-op
    tokio::time::sleep(AUTO_DISCOVER_WAIT).await;
    harness.run_pending_controller("marmots/capture").await;

    let capture_state = harness.get_controller_state("marmots/capture").await;
    let status = capture_state.current_status.unwrap_capture();
    let auto_discover = status.auto_discover.as_ref().unwrap();
    assert!(auto_discover.failure.is_none());
    let success = auto_discover.last_success.as_ref().unwrap();
    assert!(success.ts > last_success_time);
    assert!(success.added.is_empty());
    assert!(success.modified.is_empty());
    assert!(success.removed.is_empty());
    let last_success_time = success.ts;

    let discovered = Discovered {
        bindings: vec![
            Binding {
                recommended_name: "grass".to_string(),
                resource_config_json:
                    r#"{"id": "grass", "expect": "ignore in favor of existing" }"#.to_string(),
                document_schema_json: document_schema(2).to_string(),
                key: vec!["/id".to_string()],
                disable: false,
                resource_path: Vec::new(),
            },
            Binding {
                recommended_name: "flowers".to_string(),
                resource_config_json: r#"{"id": "flowers", "extra": "flowers" }"#.to_string(),
                document_schema_json: document_schema(1).to_string(),
                key: vec!["/id".to_string()],
                disable: false,
                resource_path: Vec::new(),
            },
        ],
    };
    harness
        .discover_handler
        .connectors
        .mock_discover(Box::new(Ok(discovered)));

    tokio::time::sleep(AUTO_DISCOVER_WAIT).await;
    harness.run_pending_controller("marmots/capture").await;

    let capture_state = harness.get_controller_state("marmots/capture").await;
    let status = capture_state.current_status.unwrap_capture();
    let auto_discover = status.auto_discover.as_ref().unwrap();
    assert!(auto_discover.failure.is_none());
    let success = auto_discover.last_success.as_ref().unwrap();
    assert!(success.ts > last_success_time);
    insta::assert_json_snapshot!(success, {
        ".ts" => "[ts]",
    }, @r###"
    {
      "ts": "[ts]",
      "added": [
        {
          "resource_path": [
            "flowers"
          ],
          "target": "marmots/flowers",
          "disable": false
        }
      ],
      "removed": [
        {
          "resource_path": [
            "moss"
          ],
          "target": "marmots/moss",
          "disable": true
        }
      ],
      "publish_result": {
        "type": "success"
      }
    }
    "###);
    let bindings = &capture_state
        .live_spec
        .as_ref()
        .unwrap()
        .as_capture()
        .unwrap()
        .bindings;
    insta::assert_json_snapshot!(bindings, @r###"
    [
      {
        "resource": {
          "$serde_json::private::RawValue": "{\"extra\":\"flowers\",\"id\":\"flowers\"}"
        },
        "target": "marmots/flowers"
      },
      {
        "resource": {
          "$serde_json::private::RawValue": "{\"extra\":\"grass\",\"id\":\"grass\"}"
        },
        "target": "marmots/grass"
      }
    ]
    "###);

    harness.run_pending_controllers(Some(6)).await;
    let materialization_state = harness.get_controller_state("marmots/materialize").await;
    let model = materialization_state.live_spec.as_ref().unwrap();
    let bindings = &model.as_materialization().unwrap().bindings;
    insta::assert_json_snapshot!(bindings, @r###"
    [
      {
        "resource": {
          "$serde_json::private::RawValue": "{\"id\":\"flowers\"}"
        },
        "source": "marmots/flowers",
        "fields": {
          "recommended": true
        }
      },
      {
        "resource": {
          "$serde_json::private::RawValue": "{\"id\":\"grass\"}"
        },
        "source": "marmots/grass",
        "fields": {
          "recommended": true
        }
      }
    ]
    "###);

    // Final snapshot of the publication history
    let pub_history = &capture_state
        .current_status
        .unwrap_capture()
        .publications
        .history
        .iter()
        .map(|e| (e.detail.as_ref(), &e.result))
        .collect::<Vec<_>>();
    insta::assert_json_snapshot!(pub_history, @r###"
    [
      [
        "auto-discover changes (1 added, 0 modified, 1 removed)",
        {
          "type": "success"
        }
      ],
      [
        "auto-discover changes (2 added, 0 modified, 0 removed)",
        {
          "type": "success"
        }
      ]
    ]
    "###);
}

#[tokio::test]
#[serial_test::serial]
async fn test_auto_discovers_no_evolution() {
    let mut harness = TestHarness::init("test_auto_discovers_no_evolution").await;

    let user_id = harness.setup_tenant("mules").await;

    // Start out by doing a user-initiated discover and publishing the results.
    // The discover should be merged with this spec.
    let init_draft = draft_catalog(json!({
        "captures": {
            "mules/capture": {
                "autoDiscover": {
                    "addNewBindings": false,
                    "evolveIncompatibleCollections": false,
                },
                "endpoint": {
                    "connector": {
                        "image": "source/test:test",
                        "config": { "hee": "haw" }
                    },
                },
                "bindings": [ ]
            },
        }
    }));
    let draft_id = harness
        .create_draft(user_id, "mules-init-draft", init_draft)
        .await;
    let discovered = Discovered {
        bindings: vec![Binding {
            recommended_name: "hey".to_string(),
            resource_config_json: r#"{"id": "hey"}"#.to_string(),
            document_schema_json: document_schema(1).to_string(),
            key: vec!["/id".to_string()],
            disable: false,
            resource_path: Vec::new(),
        }],
    };
    let result = harness
        .user_discover(
            "source/test",
            ":test",
            "mules/capture",
            draft_id,
            r#"{"hee": "hawwww"}"#,
            false,
            Box::new(Ok(discovered.clone())),
        )
        .await;
    assert!(result.job_status.is_success());
    let result = harness
        .create_user_publication(user_id, draft_id, "mules init_draft")
        .await;
    assert!(result.status.is_success());

    harness.run_pending_controllers(None).await;

    let new_discovered = Discovered {
        bindings: vec![Binding {
            recommended_name: "hey".to_string(),
            resource_config_json: r#"{"id": "hey"}"#.to_string(),
            document_schema_json: document_schema(1).to_string(),
            key: vec!["/id".to_string(), "/squeaks".to_string()],
            disable: false,
            resource_path: Vec::new(),
        }],
    };
    harness
        .set_auto_discover_interval("mules/capture", AUTO_DISCOVER_INTERVAL)
        .await;
    tokio::time::sleep(AUTO_DISCOVER_WAIT).await;
    harness
        .discover_handler
        .connectors
        .mock_discover(Box::new(Ok(new_discovered)));
    harness.run_pending_controller("mules/capture").await;

    let capture_state = harness.get_controller_state("mules/capture").await;
    let capture_status = capture_state.current_status.unwrap_capture();
    // Expect to see that the discover succeeded, but that the publication failed.
    insta::assert_json_snapshot!(capture_status, {
        ".activation.last_activated" => "[build_id]",
        ".auto_discover.failure.first_ts" => "[ts]",
        ".auto_discover.failure.last_outcome.ts" => "[ts]",
        ".publications.max_observed_pub_id" => "[pub_id]",
        ".publications.history[].id" => "[pub_id]",
        ".publications.history[].created" => "[ts]",
        ".publications.history[].completed" => "[ts]",
    }, @r###"
    {
      "publications": {
        "max_observed_pub_id": "[pub_id]",
        "history": [
          {
            "id": "[pub_id]",
            "created": "[ts]",
            "completed": "[ts]",
            "detail": "auto-discover changes (0 added, 1 modified, 0 removed)",
            "result": {
              "type": "buildFailed",
              "incompatible_collections": [
                {
                  "collection": "mules/hey",
                  "requires_recreation": [
                    "keyChange"
                  ]
                }
              ]
            },
            "errors": [
              {
                "catalog_name": "mules/hey",
                "scope": "flow://collection/mules/hey",
                "detail": "collection key and logical partitioning may not be changed; a new collection must be created"
              }
            ]
          }
        ]
      },
      "activation": {
        "last_activated": "[build_id]"
      },
      "auto_discover": {
        "interval": "15ms",
        "failure": {
          "count": 1,
          "first_ts": "[ts]",
          "last_outcome": {
            "ts": "[ts]",
            "modified": [
              {
                "resource_path": [
                  "hey"
                ],
                "target": "mules/hey",
                "disable": false
              }
            ],
            "publish_result": {
              "type": "buildFailed",
              "incompatible_collections": [
                {
                  "collection": "mules/hey",
                  "requires_recreation": [
                    "keyChange"
                  ]
                }
              ]
            }
          }
        }
      }
    }
    "###);

    // Now simulate the discovered key going back to normal and assert that it succeeds
    tokio::time::sleep(AUTO_DISCOVER_WAIT).await;
    harness
        .discover_handler
        .connectors
        .mock_discover(Box::new(Ok(discovered)));
    harness.run_pending_controller("mules/capture").await;

    let capture_state = harness.get_controller_state("mules/capture").await;
    let auto_discover = capture_state
        .current_status
        .unwrap_capture()
        .auto_discover
        .as_ref()
        .unwrap();
    assert!(auto_discover.last_success.is_some());
    assert!(auto_discover
        .last_success
        .as_ref()
        .unwrap()
        .publish_result
        .is_none());
    assert!(auto_discover.failure.is_none());
}

#[tokio::test]
#[serial_test::serial]
async fn test_auto_discovers_update_only() {
    let mut harness = TestHarness::init("test_auto_discovers_update_only").await;

    let user_id = harness.setup_tenant("pikas").await;

    let init_draft = draft_catalog(json!({
        "captures": {
            "pikas/capture": {
                "autoDiscover": {
                    "addNewBindings": false,
                    "evolveIncompatibleCollections": true,
                },
                "shards": {
                    "logLevel": "debug"
                },
                "interval": "42s",
                "endpoint": {
                    "connector": {
                        "image": "source/test:test",
                        "config": { "squeak": "squeak" }
                    },
                },
                "bindings": [
                    {
                        "resource": { "id": "grass", "extra": "grass" },
                        "target": "pikas/alpine-grass"
                    },
                    {
                        "resource": { "id": "moss", "extra": "moss" },
                        "target": "pikas/moss"
                    },
                    {
                        "resource": { "id": "lichen", "extra": "lichen" },
                        "target": "pikas/lichen",
                        "disable": true,
                    }
                ]
            },
            // This is just to ensure that we don't auto-discover disabled captures
            "pikas/disabled-capture": {
                "autoDiscover": {
                    "addNewBindings": false,
                    "evolveIncompatibleCollections": true,
                },
                "shards": {
                    "disable": true
                },
                "endpoint": {
                    "connector": {
                        "image": "source/test:test",
                        "config": { "squeak": "" }
                    },
                },
                "bindings": [ ]
            },
            "pikas/capture-auto-disco-disabled": {
                "autoDiscover": null,
                "shards": {
                    "disable": true
                },
                "endpoint": {
                    "connector": {
                        "image": "source/test:test",
                        "config": { "squeak": "" }
                    },
                },
                "bindings": [ ]
            },
        },
        "collections": {
            "pikas/alpine-grass": {
                "schema": document_schema(1),
                "key": ["/id"]
            },
            "pikas/moss": {
                "schema": document_schema(1),
                "key": ["/id"]
            },
            "pikas/lichen": {
                "writeSchema": document_schema(1),
                "readSchema": models::Schema::default_inferred_read_schema(),
                "key": ["/id"]
            }
        },
        "materializations": {
            "pikas/materialize": {
                "sourceCapture": "pikas/capture",
                "endpoint": {
                    "connector": {
                        "image": "materialize/test:test",
                        "config": { "squeak": "squeak squeak" }
                    }
                },
                "bindings": [] // let the materialization controller fill them in
            }
        }
    }));

    let result = harness
        .user_publication(user_id, "init publication", init_draft)
        .await;
    assert!(result.status.is_success());

    harness.run_pending_controllers(Some(6)).await;

    // Expect to see that the controller has initialized a blank auto-capture status.
    let capture_state = harness.get_controller_state("pikas/capture").await;
    assert!(capture_state.next_run.is_some());
    assert!(capture_state
        .current_status
        .unwrap_capture()
        .auto_discover
        .is_some());

    let disabled_state = harness.get_controller_state("pikas/disabled-capture").await;
    assert!(disabled_state.next_run.is_none());
    assert!(disabled_state
        .current_status
        .unwrap_capture()
        .auto_discover
        .is_none());
    let ad_disabled_state = harness
        .get_controller_state("pikas/capture-auto-disco-disabled")
        .await;
    assert!(ad_disabled_state.next_run.is_none());
    assert!(ad_disabled_state
        .current_status
        .unwrap_capture()
        .auto_discover
        .is_none());

    harness
        .set_auto_discover_interval("pikas/capture", AUTO_DISCOVER_INTERVAL)
        .await;
    tokio::time::sleep(AUTO_DISCOVER_WAIT).await;
    let discovered = Discovered {
        bindings: vec![
            Binding {
                recommended_name: "grass".to_string(),
                resource_config_json: r#"{"id": "grass"}"#.to_string(),
                document_schema_json: document_schema(2).to_string(),
                key: vec!["/id".to_string()],
                disable: true,
                resource_path: Vec::new(),
            },
            Binding {
                recommended_name: "moss".to_string(),
                resource_config_json:
                    r#"{"id": "moss", "expect": "existing config takes precedence" }"#.to_string(),
                document_schema_json: document_schema(1).to_string(),
                key: vec!["/id".to_string()],
                disable: true,
                resource_path: Vec::new(),
            },
            Binding {
                recommended_name: "lichen".to_string(),
                resource_config_json: r#"{"id": "lichen"}"#.to_string(),
                document_schema_json: document_schema(1).to_string(),
                key: vec!["/id".to_string()],
                disable: false,
                resource_path: Vec::new(),
            },
        ],
    };
    harness
        .discover_handler
        .connectors
        .mock_discover(Box::new(Ok(discovered)));

    harness.run_pending_controller("pikas/capture").await;
    let capture_state = harness.get_controller_state("pikas/capture").await;
    let auto_discover = capture_state
        .current_status
        .unwrap_capture()
        .auto_discover
        .as_ref()
        .unwrap();

    assert!(auto_discover.failure.is_none());
    assert!(auto_discover.last_success.is_some());
    let last_success = auto_discover.last_success.as_ref().unwrap();

    assert_eq!(
        &changes(&[(&["grass"], "pikas/alpine-grass", false),]),
        &last_success.modified
    );
    assert!(last_success.added.is_empty());
    assert!(last_success.removed.is_empty());
    assert!(last_success
        .publish_result
        .as_ref()
        .is_some_and(|pr| pr.is_success()));
    let last_disco_time = last_success.ts;

    // Discover again with the same response, and assert that there are no changes, and no publication.
    tokio::time::sleep(AUTO_DISCOVER_WAIT).await;
    harness.run_pending_controller("pikas/capture").await;

    let capture_state = harness.get_controller_state("pikas/capture").await;
    let auto_discover = capture_state
        .current_status
        .unwrap_capture()
        .auto_discover
        .as_ref()
        .unwrap();

    assert!(auto_discover.failure.is_none());
    assert!(auto_discover.last_success.is_some());
    let last_success = auto_discover.last_success.as_ref().unwrap();
    assert!(last_success.ts > last_disco_time);
    assert!(last_success.added.is_empty());
    assert!(last_success.modified.is_empty());
    assert!(last_success.removed.is_empty());
    assert!(last_success.publish_result.is_none());

    // Now simulate a discover error, and expect to see the error status reported.
    harness
        .discover_handler
        .connectors
        .mock_discover(Box::new(Err("a simulated discover error".to_string())));
    tokio::time::sleep(AUTO_DISCOVER_WAIT).await;
    harness.run_pending_controller("pikas/capture").await;

    let capture_state = harness.get_controller_state("pikas/capture").await;
    let auto_discover = capture_state
        .current_status
        .unwrap_capture()
        .auto_discover
        .as_ref()
        .unwrap();
    assert!(auto_discover.failure.is_some());
    let failure = auto_discover.failure.as_ref().unwrap();
    assert!(failure.last_outcome.errors[0]
        .detail
        .contains("a simulated discover error"));
    assert_eq!(1, failure.count);

    // Now simulate a subsequent successful discover, but with a failure to
    // publish. We'll expect to see the error count go up.
    let discovered = Discovered {
        bindings: vec![
            Binding {
                recommended_name: "grass".to_string(),
                resource_config_json: r#"{"id": "grass"}"#.to_string(),
                document_schema_json: document_schema(2).to_string(),
                key: vec!["/id".to_string()],
                disable: false,
                resource_path: Vec::new(),
            },
            Binding {
                recommended_name: "moss".to_string(),
                resource_config_json:
                    r#"{"id": "moss", "expect": "existing config takes precedence" }"#.to_string(),
                document_schema_json: document_schema(2).to_string(),
                key: vec!["/id".to_string()],
                disable: true,
                resource_path: Vec::new(),
            },
            // Lichens is missing, and we expect the corresponding binding to be
            // removed once a successful discover is published.
        ],
    };
    harness
        .discover_handler
        .connectors
        .mock_discover(Box::new(Ok(discovered)));
    harness.control_plane().fail_next_build(
        "pikas/capture",
        InjectBuildError::new(
            tables::synthetic_scope(models::CatalogType::Capture, "pikas/capture"),
            anyhow::anyhow!("a simulated build failure"),
        ),
    );
    tokio::time::sleep(AUTO_DISCOVER_WAIT).await;
    harness.run_pending_controller("pikas/capture").await;

    let capture_state = harness.get_controller_state("pikas/capture").await;
    let auto_discover = capture_state
        .current_status
        .unwrap_capture()
        .auto_discover
        .as_ref()
        .unwrap();
    assert!(auto_discover.failure.is_some());
    let failure = auto_discover.failure.as_ref().unwrap();
    assert_eq!(2, failure.count);
    assert_eq!(
        Some(publications::JobStatus::BuildFailed {
            incompatible_collections: Vec::new(),
            evolution_id: None
        }),
        failure.last_outcome.publish_result
    );
    // Ensure that the failed publication is shown in the history.
    let pub_history = capture_state
        .current_status
        .unwrap_capture()
        .publications
        .history
        .front()
        .unwrap();
    assert!(pub_history.errors[0]
        .detail
        .contains("a simulated build failure"));
    let last_fail_time = failure.last_outcome.ts;

    // Now this time, we'll discover a changed key, and expect that the initial publication fails
    // due to the key change, and that a subsequent publication of a _v2 collection is successful.
    let discovered = Discovered {
        bindings: vec![
            Binding {
                recommended_name: "grass".to_string(),
                resource_config_json: r#"{"id": "grass"}"#.to_string(),
                document_schema_json: document_schema(2).to_string(),
                key: vec!["/id".to_string(), "/squeaks".to_string()],
                disable: false,
                resource_path: Vec::new(),
            },
            Binding {
                recommended_name: "moss".to_string(),
                resource_config_json:
                    r#"{"id": "moss", "expect": "existing config takes precedence" }"#.to_string(),
                document_schema_json: document_schema(2).to_string(),
                key: vec!["/id".to_string()],
                disable: true,
                resource_path: Vec::new(),
            },
            // Lichens is missing, and we expect the corresponding binding to be
            // removed once a successful discover is published.
        ],
    };
    harness
        .discover_handler
        .connectors
        .mock_discover(Box::new(Ok(discovered)));
    tokio::time::sleep(AUTO_DISCOVER_WAIT).await;
    harness.run_pending_controller("pikas/capture").await;

    let capture_state = harness.get_controller_state("pikas/capture").await;
    let auto_discover = capture_state
        .current_status
        .unwrap_capture()
        .auto_discover
        .as_ref()
        .unwrap();
    let last_success = auto_discover.last_success.as_ref().unwrap();
    assert!(last_success.ts > last_fail_time);

    // Assert that the materialization binding has been backfilled for the re-created collection.
    let materialization_state = harness.get_controller_state("pikas/materialize").await;
    let model = materialization_state.live_spec.as_ref().unwrap();
    let bindings = &model.as_materialization().unwrap().bindings;
    insta::assert_json_snapshot!(bindings, @r###"
    [
      {
        "resource": {
          "$serde_json::private::RawValue": "{\"id\":\"alpine-grass\"}"
        },
        "source": "pikas/alpine-grass_v2",
        "fields": {
          "recommended": true
        },
        "backfill": 1
      },
      {
        "resource": {
          "$serde_json::private::RawValue": "{\"id\":\"moss\"}"
        },
        "source": "pikas/moss",
        "fields": {
          "recommended": true
        }
      }
    ]
    "###);

    // Final snapshot of the publication history
    let pub_history = &capture_state
        .current_status
        .unwrap_capture()
        .publications
        .history
        .iter()
        .map(|e| (e.detail.as_ref(), &e.result))
        .collect::<Vec<_>>();
    insta::assert_json_snapshot!(pub_history, @r###"
    [
      [
        "auto-discover changes (0 added, 2 modified, 1 removed), and re-creating 1 collections",
        {
          "type": "success"
        }
      ],
      [
        "auto-discover changes (0 added, 2 modified, 1 removed)",
        {
          "type": "buildFailed",
          "incompatible_collections": [
            {
              "collection": "pikas/alpine-grass",
              "requires_recreation": [
                "keyChange"
              ]
            }
          ]
        }
      ],
      [
        "auto-discover changes (0 added, 1 modified, 1 removed)",
        {
          "type": "buildFailed"
        }
      ],
      [
        "auto-discover changes (0 added, 1 modified, 0 removed)",
        {
          "type": "success"
        }
      ]
    ]
    "###);
}

fn changes(c: &[(&[&str], &str, bool)]) -> Vec<DiscoverChange> {
    c.into_iter()
        .map(|(path, target, disable)| DiscoverChange {
            resource_path: path.iter().map(|s| s.to_string()).collect(),
            target: models::Collection::new(*target),
            disable: *disable,
        })
        .collect()
}

fn document_schema(version: usize) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "id": {"type": "string"},
            "squeaks": { "type": "integer", "maximum": version },
        },
        "required": ["id", "squeaks"]
    })
}