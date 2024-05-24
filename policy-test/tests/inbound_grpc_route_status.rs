#![allow(dead_code, unused_imports)]

use kube::ResourceExt;
use linkerd_policy_controller_k8s_api::{
    self as k8s, gateway as k8s_gateway_api, policy as linkerd_k8s_api,
};
use linkerd_policy_test::{await_condition, create, find_route_condition, update, with_temp_ns};

fn mk_route(
    ns: &str,
    name: &str,
    parent_refs: Option<Vec<k8s_gateway_api::ParentReference>>,
) -> k8s::gateway::GrpcRoute {
    k8s::gateway::GrpcRoute {
        metadata: kube::api::ObjectMeta {
            namespace: Some(ns.to_string()),
            name: Some(name.to_string()),
            ..Default::default()
        },
        spec: k8s::gateway::GrpcRouteSpec {
            inner: k8s::gateway::CommonRouteSpec { parent_refs },
            hostnames: None,
            rules: Some(vec![]),
        },
        status: None,
    }
}

// Waits until a GrpcRoute with the given namespace and name has a status set
// on it, then returns the generic route status representation.
pub async fn await_route_status(
    client: &kube::Client,
    ns: &str,
    name: &str,
) -> k8s_gateway_api::RouteStatus {
    let route_status = await_condition(
        client,
        ns,
        name,
        |obj: Option<&k8s::gateway::GrpcRoute>| -> bool {
            obj.and_then(|route| route.status.as_ref()).is_some()
        },
    )
    .await
    .expect("must fetch route")
    .status
    .expect("route must contain a status representation")
    .inner;

    tracing::trace!(?route_status, name, ns, "got route status");

    route_status
}

#[tokio::test(flavor = "current_thread")]
async fn inbound_accepted_parent() {
    with_temp_ns(|client, ns| async move {
        // Create a test 'Server'
        let server_name = "test-accepted-server";

        let server = linkerd_k8s_api::Server {
            metadata: k8s::ObjectMeta {
                namespace: Some(ns.to_string()),
                name: Some(server_name.to_string()),
                ..Default::default()
            },
            spec: linkerd_k8s_api::ServerSpec {
                selector: linkerd_k8s_api::server::Selector::Pod(k8s::labels::Selector::from_iter(
                    Some(("app", server_name)),
                )),
                port: linkerd_k8s_api::server::Port::Name("grpc".to_string()),
                proxy_protocol: Some(linkerd_k8s_api::server::ProxyProtocol::Grpc),
            },
        };

        let server = create(&client, server).await;

        let srv_ref = vec![k8s_gateway_api::ParentReference {
            group: Some("policy.linkerd.io".to_string()),
            kind: Some("Server".to_string()),
            namespace: server.namespace(),
            name: server.name_unchecked(),
            section_name: None,
            port: None,
        }];

        // Create a route that references the Server resource.
        let _route = create(&client, mk_route(&ns, "test-accepted-route", Some(srv_ref))).await;

        // Wait until route is updated with a status
        let statuses = await_route_status(&client, &ns, "test-accepted-route")
            .await
            .parents;

        let route_status = statuses
            .clone()
            .into_iter()
            .find(|route_status| route_status.parent_ref.name == server_name)
            .expect("must have at least one parent status");

        // Check status references to parent we have created
        assert_eq!(
            route_status.parent_ref.group.as_deref(),
            Some("policy.linkerd.io")
        );

        assert_eq!(route_status.parent_ref.kind.as_deref(), Some("Server"));

        // Check status is accepted with a status of 'True'
        let cond = find_route_condition(&statuses, server_name)
            .expect("must have at least one 'Accepted' condition for accepted server");

        assert_eq!(cond.status, "True");
        assert_eq!(cond.reason, "Accepted")
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn inbound_multiple_parents() {
    with_temp_ns(|client, ns| async move {
        // Exercise accepted test with a valid, and an invalid parent reference
        let srv_refs = vec![
            k8s_gateway_api::ParentReference {
                group: Some("policy.linkerd.io".to_string()),
                kind: Some("Server".to_string()),
                namespace: Some(ns.clone()),
                name: "test-valid-server".to_string(),
                section_name: None,
                port: None,
            },
            k8s_gateway_api::ParentReference {
                group: Some("policy.linkerd.io".to_string()),
                kind: Some("Server".to_string()),
                namespace: Some(ns.clone()),
                name: "test-invalid-server".to_string(),
                section_name: None,
                port: None,
            },
        ];

        // Create only one of the parents
        let server = linkerd_k8s_api::Server {
            metadata: k8s::ObjectMeta {
                namespace: Some(ns.to_string()),
                name: Some("test-valid-server".to_string()),
                ..Default::default()
            },
            spec: linkerd_k8s_api::ServerSpec {
                selector: linkerd_k8s_api::server::Selector::Pod(k8s::labels::Selector::from_iter(
                    Some(("app", "test-valid-server")),
                )),
                port: linkerd_k8s_api::server::Port::Name("grpc".to_string()),
                proxy_protocol: Some(linkerd_k8s_api::server::ProxyProtocol::Grpc),
            },
        };

        let _server = create(&client, server).await;

        // Create a route that references both parents.
        let _route = create(
            &client,
            mk_route(&ns, "test-multiple-parents-route", Some(srv_refs)),
        )
        .await;

        // Wait until route is updated with a status
        let parent_status = await_route_status(&client, &ns, "test-multiple-parents-route")
            .await
            .parents;

        // Find status for invalid parent and extract the condition
        let invalid_cond = find_route_condition(&parent_status, "test-invalid-server")
            .expect("must have at least one 'Accepted' condition set for invalid parent");

        // Route shouldn't be accepted
        assert_eq!(invalid_cond.status, "False");
        assert_eq!(invalid_cond.reason, "NoMatchingParent");

        // Find status for valid parent and extract the condition
        let valid_cond = find_route_condition(&parent_status, "test-valid-server")
            .expect("must have at least one 'Accepted' condition set for valid parent");

        assert_eq!(valid_cond.status, "True");
        assert_eq!(valid_cond.reason, "Accepted")
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
async fn inbound_no_parent_ref_patch() {
    with_temp_ns(|client, ns| async move {
        // Create a test 'Server'
        let server_name = "test-accepted-server";
        let server = linkerd_k8s_api::Server {
            metadata: k8s::ObjectMeta {
                namespace: Some(ns.to_string()),
                name: Some(server_name.to_string()),
                ..Default::default()
            },
            spec: linkerd_k8s_api::ServerSpec {
                selector: linkerd_k8s_api::server::Selector::Pod(k8s::labels::Selector::from_iter(
                    Some(("app", server_name)),
                )),
                port: linkerd_k8s_api::server::Port::Name("grpc".to_string()),
                proxy_protocol: Some(linkerd_k8s_api::server::ProxyProtocol::Grpc),
            },
        };

        let server = create(&client, server).await;

        let srv_ref = vec![k8s_gateway_api::ParentReference {
            group: Some("policy.linkerd.io".to_string()),
            kind: Some("Server".to_string()),
            namespace: server.namespace(),
            name: server.name_unchecked(),
            section_name: None,
            port: None,
        }];

        // Create a route with a parent reference.
        let route = create(
            &client,
            mk_route(&ns, "test-no-parent-refs-route", Some(srv_ref)),
        )
        .await;

        // Status may not be set straight away. To account for that, wrap a
        // status condition watcher in a timeout.
        let status = await_route_status(&client, &ns, "test-no-parent-refs-route").await;

        // If timeout has elapsed, then route did not receive a status patch
        assert_eq!(
            status.parents.len(),
            1,
            "GRPCRoute Status should have 1 parent status"
        );

        // Update route to remove parent_refs
        let _route = update(&client, mk_route(&ns, "test-no-parent-refs-route", None)).await;

        // Wait for the status to be updated to contain no parent statuses.
        await_condition::<k8s_gateway_api::GrpcRoute>(
            &client,
            &ns,
            &route.name_unchecked(),
            |obj: Option<&k8s_gateway_api::GrpcRoute>| -> bool {
                obj.and_then(|route| route.status.as_ref())
                    .is_some_and(|status| status.inner.parents.is_empty())
            },
        )
        .await
        .expect("GRPCRoute Status should have no parent status");
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
// Tests that inbound routes (routes attached to a `Server`) are properly
// reconciled when the parentReference changes. Additionally, tests that routes
// whose parentRefs do not exist are patched with an appropriate status.
async fn inbound_accepted_reconcile_no_parent() {
    with_temp_ns(|client, ns| async move {
        // Given a route with a nonexistent parentReference, we expect to have an
        // 'Accepted' condition with 'False' as a status.
        let server_name = "test-reconcile-inbound-server";

        let srv_ref = vec![k8s_gateway_api::ParentReference {
            group: Some("policy.linkerd.io".to_string()),
            kind: Some("Server".to_string()),
            namespace: Some(ns.clone()),
            name: server_name.to_string(),
            section_name: None,
            port: None,
        }];

        let _route = create(
            &client,
            mk_route(&ns, "test-reconcile-inbound-route", Some(srv_ref)),
        )
        .await;

        let route_status = await_route_status(&client, &ns, "test-reconcile-inbound-route").await;

        let cond = find_route_condition(&route_status.parents, server_name)
            .expect("must have at least one 'Accepted' condition set for parent");

        // Test when parent ref does not exist we get Accepted { False }.
        assert_eq!(cond.status, "False");
        assert_eq!(cond.reason, "NoMatchingParent");

        // Create the 'Server' that route references and expect it to be picked up
        // by the index. Consequently, route will have its status reconciled.
        let server = linkerd_k8s_api::Server {
            metadata: k8s::ObjectMeta {
                namespace: Some(ns.to_string()),
                name: Some(server_name.to_string()),
                ..Default::default()
            },
            spec: linkerd_k8s_api::ServerSpec {
                selector: linkerd_k8s_api::server::Selector::Pod(k8s::labels::Selector::from_iter(
                    Some(("app", server_name)),
                )),
                port: linkerd_k8s_api::server::Port::Name("grpc".to_string()),
                proxy_protocol: Some(linkerd_k8s_api::server::ProxyProtocol::Grpc),
            },
        };

        create(&client, server).await;

        // GRPCRoute may not be patched instantly, await the route condition
        // status becoming accepted.
        let _route_status = await_condition(
            &client,
            &ns,
            "test-reconcile-inbound-route",
            |obj: Option<&k8s_gateway_api::GrpcRoute>| -> bool {
                tracing::trace!(?obj, "got route status");
                let status = match obj.and_then(|route| route.status.as_ref()) {
                    Some(status) => status,
                    None => return false,
                };
                let cond = match find_route_condition(&status.inner.parents, server_name) {
                    Some(cond) => cond,
                    None => return false,
                };
                cond.status == "True" && cond.reason == "Accepted"
            },
        )
        .await
        .expect("must fetch route")
        .status
        .expect("route must contain a status representation");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn inbound_accepted_reconcile_parent_delete() {
    with_temp_ns(|client, ns| async move {
        // Attach a route to a Server and expect the route to be patched with an
        // Accepted status.
        let server_name = "test-reconcile-delete-server";

        let server = linkerd_k8s_api::Server {
            metadata: k8s::ObjectMeta {
                namespace: Some(ns.to_string()),
                name: Some(server_name.to_string()),
                ..Default::default()
            },
            spec: linkerd_k8s_api::ServerSpec {
                selector: linkerd_k8s_api::server::Selector::Pod(k8s::labels::Selector::from_iter(
                    Some(("app", server_name)),
                )),
                port: linkerd_k8s_api::server::Port::Name("grpc".to_string()),
                proxy_protocol: Some(linkerd_k8s_api::server::ProxyProtocol::Grpc),
            },
        };

        create(&client, server).await;

        // Create parentReference and route
        let srv_ref = vec![k8s_gateway_api::ParentReference {
            group: Some("policy.linkerd.io".to_string()),
            kind: Some("Server".to_string()),
            namespace: Some(ns.clone()),
            name: server_name.to_string(),
            section_name: None,
            port: None,
        }];

        let _route = create(
            &client,
            mk_route(&ns, "test-reconcile-delete-route", Some(srv_ref)),
        )
        .await;

        let route_status = await_route_status(&client, &ns, "test-reconcile-delete-route").await;

        let cond = find_route_condition(&route_status.parents, server_name)
            .expect("must have at least one 'Accepted' condition");

        assert_eq!(cond.status, "True");
        assert_eq!(cond.reason, "Accepted");

        // Delete Server
        let api: kube::Api<linkerd_k8s_api::Server> = kube::Api::namespaced(client.clone(), &ns);

        api.delete(
            "test-reconcile-delete-server",
            &kube::api::DeleteParams::default(),
        )
        .await
        .expect("API delete request failed");

        // GRPCRoute may not be patched instantly, await the route condition
        // becoming NoMatchingParent.
        let _route_status = await_condition(
            &client,
            &ns,
            "test-reconcile-delete-route",
            |obj: Option<&k8s_gateway_api::GrpcRoute>| -> bool {
                tracing::trace!(?obj, "got route status");
                let status = match obj.and_then(|route| route.status.as_ref()) {
                    Some(status) => status,
                    None => return false,
                };
                let cond = match find_route_condition(&status.inner.parents, server_name) {
                    Some(cond) => cond,
                    None => return false,
                };
                cond.status == "False" && cond.reason == "NoMatchingParent"
            },
        )
        .await
        .expect("must fetch route")
        .status
        .expect("route must contain a status representation");
    })
    .await;
}
