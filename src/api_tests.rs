//! Client and context behaviour against a mock Fenders API.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use httpmock::HttpMockRequest;
use httpmock::prelude::*;
use serde_json::json;

use crate::app::{App, revision_of};
use crate::client::{ApiError, Client};
use crate::config::Config;
use crate::output::Out;

const WS: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const PJ: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const OP: &str = "cccccccccccccccccccccccccccccccc";
const APP: &str = "dddddddddddddddddddddddddddddddd";

fn client(server: &MockServer) -> Client {
    Client::new(&server.base_url(), Some("test-key".into()), false)
        .unwrap()
        .with_retry_base(Duration::from_millis(1))
}

fn app(server: &MockServer) -> App {
    let cfg = Config {
        workspace: Some(WS.into()),
        project: Some(PJ.into()),
        ..Default::default()
    };
    App::new(
        client(server),
        Out { json: true },
        cfg,
        server.base_url(),
        None,
        None,
    )
}

fn proj() -> String {
    format!("/workspaces/{WS}/projects/{PJ}")
}

#[test]
fn sends_bearer_and_parses_json() {
    let server = MockServer::start();
    let m = server.mock(|when, then| {
        when.method(GET)
            .path("/me")
            .header("authorization", "Bearer test-key");
        then.status(200)
            .json_body(json!({"id": "k1", "name": "ci"}));
    });
    let v = client(&server).get("/me", &[]).unwrap();
    m.assert();
    assert_eq!(v["name"], "ci");
}

#[test]
fn maps_error_schema() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/workspaces");
        then.status(403).json_body(json!({
            "error": "permission_denied",
            "code": "forbidden",
            "request_id": "0123456789abcdef0123456789abcdef"
        }));
    });
    let err = client(&server).get("/workspaces", &[]).unwrap_err();
    let api = err.downcast_ref::<ApiError>().expect("ApiError");
    assert_eq!(api.status, 403);
    assert_eq!(api.code, "forbidden");
    assert_eq!(
        api.request_id.as_deref(),
        Some("0123456789abcdef0123456789abcdef")
    );
    assert_eq!(api.to_string(), "Permission denied (forbidden, HTTP 403)");
    assert!(api.hint().unwrap().contains("permission"));
}

#[test]
fn not_logged_in_fails_without_a_request() {
    let server = MockServer::start();
    let m = server.mock(|when, then| {
        when.any_request();
        then.status(200);
    });
    let c = Client::new(&server.base_url(), None, false).unwrap();
    let err = c.get("/me", &[]).unwrap_err();
    assert_eq!(err.downcast_ref::<ApiError>().unwrap().status, 401);
    m.assert_calls(0);
}

/// Record the Idempotency-Key of every request a mock sees.
fn key_recorder() -> (
    Arc<Mutex<HashSet<String>>>,
    impl Fn(&HttpMockRequest) -> bool + Send + Sync + 'static,
) {
    let keys = Arc::new(Mutex::new(HashSet::new()));
    let k = keys.clone();
    let matcher = move |req: &HttpMockRequest| {
        if let Some(v) = req.headers().get("idempotency-key") {
            k.lock().unwrap().insert(v.to_str().unwrap().to_string());
        }
        true
    };
    (keys, matcher)
}

#[test]
fn retries_reuse_one_idempotency_key() {
    let server = MockServer::start();
    let (keys, matcher) = key_recorder();
    let m = server.mock(|when, then| {
        when.method(POST)
            .path("/thing")
            .header_exists("idempotency-key")
            .is_true(matcher);
        then.status(503)
            .json_body(json!({"error": "busy", "code": "unavailable", "request_id": "x"}));
    });
    let err = client(&server).post("/thing", json!({})).unwrap_err();
    m.assert_calls(3);
    assert_eq!(
        keys.lock().unwrap().len(),
        1,
        "all retries must carry the same key"
    );
    assert_eq!(err.downcast_ref::<ApiError>().unwrap().status, 503);
}

#[test]
fn distinct_requests_get_distinct_idempotency_keys() {
    let server = MockServer::start();
    let (keys, matcher) = key_recorder();
    server.mock(|when, then| {
        when.method(POST).is_true(matcher);
        then.status(202)
            .json_body(json!({"id": OP, "state": "pending"}));
    });
    let c = client(&server);
    c.post("/artifacts", json!({})).unwrap();
    c.post("/apps/deploy", json!({})).unwrap();
    assert_eq!(
        keys.lock().unwrap().len(),
        2,
        "upload and deploy must not share a key"
    );
}

#[test]
fn truncates_an_oversized_final_page() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/v");
        then.status(200)
            .json_body(json!({"items": [{"id": "1"}, {"id": "2"}, {"id": "3"}], "next": null}));
    });
    let (items, more) = app(&server)
        .collect("/v", vec![], "before", None, Some(2))
        .unwrap();
    assert_eq!(items.len(), 2);
    assert!(more);
}

#[test]
fn does_not_retry_client_errors() {
    let server = MockServer::start();
    let m = server.mock(|when, then| {
        when.method(POST).path("/thing");
        then.status(422)
            .json_body(json!({"error": "bad", "code": "invalid", "request_id": "x"}));
    });
    assert!(client(&server).post("/thing", json!({})).is_err());
    m.assert_calls(1);
}

#[test]
fn paginates_with_string_cursor() {
    let server = MockServer::start();
    let path = format!("{}/apps", proj());
    let p1 = server.mock(|when, then| {
        when.method(GET).path(&path).query_param_missing("before");
        then.status(200)
            .json_body(json!({"items": [{"id": "1"}, {"id": "2"}], "next": "c1"}));
    });
    let p2 = server.mock(|when, then| {
        when.method(GET).path(&path).query_param("before", "c1");
        then.status(200)
            .json_body(json!({"items": [{"id": "3"}], "next": null}));
    });
    let a = app(&server);
    let (items, more) = a.collect(&path, vec![], "before", Some(100), None).unwrap();
    assert_eq!(items.len(), 3);
    assert!(!more);
    p1.assert();
    p2.assert();
}

#[test]
fn stops_at_the_requested_limit() {
    let server = MockServer::start();
    let path = format!("{}/apps", proj());
    let m = server.mock(|when, then| {
        when.method(GET).path(&path).query_param("limit", "2");
        then.status(200)
            .json_body(json!({"items": [{"id": "1"}, {"id": "2"}], "next": "c1"}));
    });
    let (items, more) = app(&server)
        .collect(&path, vec![], "before", Some(100), Some(2))
        .unwrap();
    assert_eq!(items.len(), 2);
    assert!(more);
    m.assert_calls(1);
}

#[test]
fn paginates_with_integer_cursor_and_no_limit_param() {
    let server = MockServer::start();
    let path = format!("{}/apps/{APP}/versions", proj());
    let p1 = server.mock(|when, then| {
        when.method(GET)
            .path(&path)
            .query_param_missing("before")
            .query_param_missing("limit");
        then.status(200)
            .json_body(json!({"items": [{"id": "v9"}], "next": 5}));
    });
    let p2 = server.mock(|when, then| {
        when.method(GET).path(&path).query_param("before", "5");
        then.status(200)
            .json_body(json!({"items": [{"id": "v4"}], "next": null}));
    });
    let (items, _) = app(&server)
        .collect(&path, vec![], "before", None, None)
        .unwrap();
    assert_eq!(items.len(), 2);
    p1.assert();
    p2.assert();
}

#[test]
fn waits_for_operation_success() {
    let server = MockServer::start();
    let m = server.mock(|when, then| {
        when.method(GET).path(format!("{}/operations/{OP}", proj()));
        then.status(200).json_body(
            json!({"id": OP, "kind": "deploy", "resource_id": APP, "state": "succeeded"}),
        );
    });
    let mut a = app(&server);
    let pending = json!({"id": OP, "kind": "deploy", "resource_id": APP, "state": "pending"});
    let done = a
        .wait_op(&pending, "Deploy", Duration::from_secs(10))
        .unwrap();
    assert_eq!(done["state"], "succeeded");
    m.assert();
}

#[test]
fn reports_operation_failure() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path(format!("{}/operations/{OP}", proj()));
        then.status(200).json_body(
            json!({"id": OP, "kind": "deploy", "resource_id": APP, "state": "failed", "error": "component trapped"}),
        );
    });
    let mut a = app(&server);
    let pending = json!({"id": OP, "kind": "deploy", "resource_id": APP, "state": "pending"});
    let err = a
        .wait_op(&pending, "Deploy", Duration::from_secs(10))
        .unwrap_err();
    assert!(format!("{err:#}").contains("component trapped"));
}

#[test]
fn resolves_app_names_to_ids() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path(format!("{}/apps", proj())).query_param("search", "hello");
        then.status(200).json_body(json!({
            "items": [{"id": "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee", "name": "hello-world"}, {"id": APP, "name": "hello"}],
            "next": null
        }));
    });
    let mut a = app(&server);
    assert_eq!(a.app_id("hello").unwrap(), APP);
    assert_eq!(a.app_id(APP).unwrap(), APP);
    assert!(a.app_id("nope").is_err());
}

#[test]
fn finds_revisions() {
    assert_eq!(revision_of(&json!({"revision": 4})), Some(4));
    assert_eq!(revision_of(&json!({"app": {"revision": 7}})), Some(7));
    assert_eq!(revision_of(&json!({"name": "x"})), None);
}

#[test]
fn never_sends_the_api_key_to_another_origin() {
    let server = MockServer::start();
    let m = server.mock(|when, then| {
        when.any_request();
        then.status(200).json_body(json!({}));
    });
    let c = client(&server);
    for evil in [
        "https://attacker.example/collect",
        "HTTP://attacker.example/collect",
        "//attacker.example/collect",
        "http://127.0.0.1:1/other-port",
    ] {
        let err = c.get(evil, &[]).unwrap_err().to_string();
        assert!(
            err.contains("refusing to send the API key"),
            "{evil}: {err}"
        );
    }
    m.assert_calls(0);
    // The API's own origin is still allowed as an absolute URL.
    c.get(&format!("{}/me", server.base_url()), &[]).unwrap();
    m.assert_calls(1);
}

#[test]
fn context_reflects_workspace_and_project_overrides() {
    let server = MockServer::start();
    let other_ws = "9".repeat(32);
    let other_pj = "8".repeat(32);
    server.mock(|when, then| {
        when.method(GET).path("/workspaces");
        then.status(200).json_body(json!([
            {"id": WS, "name": "acme"},
            {"id": other_ws, "name": "acme-labs"}
        ]));
    });
    server.mock(|when, then| {
        when.method(GET)
            .path(format!("/workspaces/{other_ws}/projects"));
        then.status(200)
            .json_body(json!([{"id": other_pj, "name": "staging"}]));
    });
    let cfg = Config {
        workspace: Some(WS.into()),
        project: Some(PJ.into()),
        ..Default::default()
    };
    let mk = |ws: Option<&str>, pj: Option<&str>| {
        App::new(
            client(&server),
            Out { json: true },
            cfg.clone(),
            server.base_url(),
            ws.map(Into::into),
            pj.map(Into::into),
        )
    };

    // No overrides: the saved context.
    let (ws, pj) = mk(None, None).effective_context();
    assert_eq!(ws.unwrap(), (WS.to_string(), None, false));
    assert_eq!(pj.unwrap().0, PJ);

    // -w by name: resolved, and the saved project (from another workspace) no longer applies.
    let (ws, pj) = mk(Some("acme-labs"), None).effective_context();
    assert_eq!(
        ws.unwrap(),
        (other_ws.clone(), Some("acme-labs".into()), true)
    );
    assert!(pj.is_none());

    // -w and -p together.
    let (_, pj) = mk(Some("acme-labs"), Some("staging")).effective_context();
    assert_eq!(pj.unwrap(), (other_pj, Some("staging".into()), true));
}

mod starting_apps {
    use super::*;
    use crate::cmd::WaitArgs;
    use crate::cmd::apps::{AppsCmd, ReleasesCmd, apps, releases};

    const V1: &str = "11111111111111111111111111111111";
    const V2: &str = "22222222222222222222222222222222";

    fn wait() -> WaitArgs {
        WaitArgs {
            no_wait: false,
            timeout: 10,
        }
    }

    fn draft_app(server: &MockServer) {
        server.mock(|when, then| {
            when.method(GET).path(format!("{}/apps/{APP}", proj()));
            then.status(200).json_body(json!({
                "id": APP, "name": "http-demo", "state": "draft", "revision": 1,
                "active_release_id": null, "target_release_id": null,
                "latest_code": {"id": V2, "number": 2},
                "triggers": {"http": true, "manual": false, "schedule": null, "revision": 1}
            }));
        });
    }

    fn ops_succeed(server: &MockServer) {
        server.mock(|when, then| {
            when.method(GET).path(format!("{}/operations/{OP}", proj()));
            then.status(200).json_body(
                json!({"id": OP, "kind": "x", "resource_id": APP, "state": "succeeded"}),
            );
        });
    }

    #[test]
    fn start_without_release_explains_instead_of_calling_the_api() {
        let server = MockServer::start();
        draft_app(&server);
        let patch = server.mock(|when, then| {
            when.method(PATCH);
            then.status(409);
        });
        let mut a = app(&server);
        let err = apps(
            &mut a,
            AppsCmd::Start {
                app: APP.into(),
                release: false,
                expected_revision: None,
                wait: wait(),
            },
        )
        .unwrap_err();
        let h = err
            .downcast_ref::<crate::app::Hinted>()
            .expect("hinted error");
        assert!(h.message.contains("never been released"));
        assert!(h.hints.iter().any(|x| x.contains("--release")));
        patch.assert_calls(0);
    }

    #[test]
    fn start_with_release_releases_latest_version_then_starts() {
        let server = MockServer::start();
        draft_app(&server);
        ops_succeed(&server);
        let release = server.mock(|when, then| {
            when.method(POST)
                .path(format!("{}/apps/{APP}/releases", proj()))
                .json_body_includes(format!(
                    r#"{{"version_id": "{V2}", "expected_revision": 1}}"#
                ));
            then.status(202).json_body(
                json!({"id": OP, "kind": "release", "resource_id": APP, "state": "pending"}),
            );
        });
        let start = server.mock(|when, then| {
            when.method(PATCH)
                .path(format!("{}/apps/{APP}", proj()))
                .json_body_includes(r#"{"desired": "enabled"}"#);
            then.status(202).json_body(
                json!({"id": OP, "kind": "start", "resource_id": APP, "state": "pending"}),
            );
        });
        let mut a = app(&server);
        apps(
            &mut a,
            AppsCmd::Start {
                app: APP.into(),
                release: true,
                expected_revision: None,
                wait: wait(),
            },
        )
        .unwrap();
        release.assert();
        start.assert();
    }

    #[test]
    fn release_accepts_a_version_number() {
        let server = MockServer::start();
        draft_app(&server);
        ops_succeed(&server);
        server.mock(|when, then| {
            when.method(GET)
                .path(format!("{}/apps/{APP}/versions", proj()));
            then.status(200).json_body(json!({
                "items": [{"id": V2, "number": 2}, {"id": V1, "number": 1}], "next": null
            }));
        });
        let release = server.mock(|when, then| {
            when.method(POST)
                .path(format!("{}/apps/{APP}/releases", proj()))
                .json_body_includes(format!(r#"{{"version_id": "{V1}"}}"#));
            then.status(202).json_body(
                json!({"id": OP, "kind": "release", "resource_id": APP, "state": "pending"}),
            );
        });
        let mut a = app(&server);
        releases(
            &mut a,
            ReleasesCmd::Create {
                app: APP.into(),
                version_id: Some("1".into()),
                config: Default::default(),
                expected_revision: None,
                wait: wait(),
            },
        )
        .unwrap();
        release.assert();
    }
}

#[test]
fn conflict_hints_match_the_conflict() {
    let err = |code: &str, message: &str| ApiError {
        status: 409,
        code: code.into(),
        message: message.into(),
        request_id: None,
    };
    assert!(
        err("conflict", "App has no active release")
            .hint()
            .unwrap()
            .contains("current state")
    );
    assert!(
        err("revision_mismatch", "stale")
            .hint()
            .unwrap()
            .contains("revision mismatch")
    );
}

fn deploy_options(timeout: u64) -> crate::cmd::deploy::DeployOpts {
    use clap::Parser;
    #[derive(Parser)]
    struct Options {
        #[command(flatten)]
        deploy: crate::cmd::deploy::DeployOpts,
    }
    let mut opts = Options::parse_from(["test", "--no-wait", "--skip-inspection"]).deploy;
    opts.wait.timeout = timeout;
    opts
}

#[test]
fn deploy_waits_for_validation_even_with_no_wait_and_skip_inspection() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let server = MockServer::start();
    let artifact = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
    let polls = Arc::new(AtomicUsize::new(0));
    server.mock(|when, then| {
        when.method(GET).path(format!("{}/apps", proj()));
        then.status(200).json_body(json!({"items": []}));
    });
    let count = polls.clone();
    let inspection = server.mock(|when, then| {
        when.method(GET)
            .path(format!("{}/artifacts/{artifact}/inspection", proj()));
        then.respond_with(move |_| {
            let body = match count.fetch_add(1, Ordering::SeqCst) {
                0 => json!({"state":"pending","error":"compiler-unavailable","inspection":null}),
                1 => json!({"state":"validating","error":null,"inspection":null}),
                _ => {
                    json!({"state":"valid","error":null,"inspection":{"http":true,"command":false}})
                }
            };
            httpmock::HttpMockResponse::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(body.to_string())
                .build()
        });
    });
    let count = polls.clone();
    let deployment = server.mock(|when, then| {
        when.method(POST)
            .path(format!("{}/apps/deploy", proj()))
            .json_body_includes(json!({"triggers":{"http":true,"manual":false}}).to_string());
        then.respond_with(move |_| {
            let status = if count.load(Ordering::SeqCst) >= 3 {
                202
            } else {
                409
            };
            httpmock::HttpMockResponse::builder()
                .status(status)
                .header("content-type", "application/json")
                .body(json!({"id":OP,"state":"pending","resource_id":APP}).to_string())
                .build()
        });
    });
    crate::cmd::deploy::run_apps(
        &mut app(&server),
        crate::cmd::deploy::AppsDeployArgs {
            name: "first-upload".into(),
            artifact: artifact.into(),
            opts: deploy_options(10),
        },
    )
    .unwrap();
    inspection.assert_calls(3);
    deployment.assert_calls(1);
}

#[test]
fn deploy_never_submits_on_unvalidated_or_unreadable_inspection() {
    for (status, body, expected) in [
        (
            200,
            json!({"state":"pending","error":"compiler-unavailable"}),
            "timed out",
        ),
        (200, json!({"state":"validating"}), "timed out"),
        (
            200,
            json!({"state":"rejected","error":"unsupported-import"}),
            "unsupported-import",
        ),
        (
            200,
            json!({"state":"unknown"}),
            "unexpected component validation state",
        ),
        (
            403,
            json!({"error":"forbidden","code":"forbidden"}),
            "could not inspect",
        ),
    ] {
        let server = MockServer::start();
        let artifact = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        server.mock(|when, then| {
            when.method(GET).path(format!("{}/apps", proj()));
            then.status(200).json_body(json!({"items":[]}));
        });
        let inspection = server.mock(|when, then| {
            when.method(GET)
                .path(format!("{}/artifacts/{artifact}/inspection", proj()));
            then.status(status).json_body(body);
        });
        let deployment = server.mock(|when, then| {
            when.method(POST).path(format!("{}/apps/deploy", proj()));
            then.status(202)
                .json_body(json!({"id":OP,"state":"pending"}));
        });
        let err = crate::cmd::deploy::run_apps(
            &mut app(&server),
            crate::cmd::deploy::AppsDeployArgs {
                name: "first-upload".into(),
                artifact: artifact.into(),
                opts: deploy_options(1),
            },
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains(expected), "{err:#}");
        assert!(inspection.calls() >= 1);
        deployment.assert_calls(0);
    }
}

#[test]
fn validation_deadline_prevents_late_success_retry_after_and_zero_budget_deploys() {
    use std::time::Instant;
    for (status, delay, timeout, expected_reads) in [
        (200, Duration::from_secs(3), 1, 1),
        (503, Duration::ZERO, 1, 1),
        (200, Duration::ZERO, 0, 0),
    ] {
        let server = MockServer::start();
        let artifact = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        server.mock(|when, then| {
            when.method(GET).path(format!("{}/apps", proj()));
            then.status(200).json_body(json!({"items":[]}));
        });
        let inspection = server.mock(|when, then| {
            when.method(GET)
                .path(format!("{}/artifacts/{artifact}/inspection", proj()));
            then.status(status)
                .delay(delay)
                .header("retry-after", "10")
                .json_body(json!({"state":"valid","inspection":{"http":true,"command":false}}));
        });
        let deployment = server.mock(|when, then| {
            when.method(POST).path(format!("{}/apps/deploy", proj()));
            then.status(202)
                .json_body(json!({"id":OP,"state":"pending"}));
        });
        let started = Instant::now();
        let err = crate::cmd::deploy::run_apps(
            &mut app(&server),
            crate::cmd::deploy::AppsDeployArgs {
                name: "late-validation".into(),
                artifact: artifact.into(),
                opts: deploy_options(timeout),
            },
        )
        .unwrap_err();
        assert!(
            started.elapsed() < Duration::from_millis(2500),
            "deadline was not bounded"
        );
        assert!(format!("{err:#}").contains("timed out"), "{err:#}");
        inspection.assert_calls(expected_reads);
        deployment.assert_calls(0);
    }
}

#[test]
fn request_deadline_includes_response_body_reads() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::time::Instant;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = BufReader::new(stream.try_clone().unwrap());
        loop {
            let mut line = String::new();
            if request.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                break;
            }
        }
        let body = r#"{"state":"valid"}"#;
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
        stream.write_all(&body.as_bytes()[..1]).unwrap();
        stream.flush().unwrap();
        std::thread::sleep(Duration::from_secs(3));
        let _ = stream.write_all(&body.as_bytes()[1..]);
    });
    let client = Client::new(&format!("http://{address}"), Some("test-key".into()), false).unwrap();
    let started = Instant::now();
    let result = client.get_until("/inspection", &[], started + Duration::from_secs(1));
    let elapsed = started.elapsed();
    server.join().unwrap();
    assert!(result.is_err(), "late valid response escaped the deadline");
    assert!(
        elapsed < Duration::from_millis(2500),
        "body read was not bounded: {elapsed:?}"
    );
}
