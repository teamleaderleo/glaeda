# ADR 0023: macOS hostile-CI egress boundary

Status: accepted for the first disposable-worker production path.

## Context

The disposable Lima/VZ guest is the hostile-workload boundary, but ordinary CI still needs
outbound DNS, source, package, and artifact access. Repository code may control the guest and must
not be trusted to preserve guest firewall rules.

Lima 2.2.0 always attaches its default gVisor user-mode network when no named user network is
selected. `networks: []` therefore does not remove networking. The gVisor network opens translated
host sockets from the Lima process and maps the guest gateway to host loopback. A packet-filter rule
for the interactive operator account cannot distinguish those sockets from the operator's normal
traffic, and allowing the account generic loopback access would expose host services through the
guest gateway.

## Decision

The first production installation runs the SmolRunner controller, Lima, and its in-process gVisor
network under one dedicated, non-login macOS service account. Long-lived GitHub credentials belong
to that service identity and remain outside guests. The existing operator LaunchAgent is retained
only as a development and lifecycle-integration path; it is not accepted for hostile repositories.

macOS PF is the mature enforcement component. A root-owned installation manages one fixed anchor
that applies to TCP and UDP sockets owned by the dedicated service UID. The anchor:

- permits the one fixed loopback TCP port used only by Lima's SSH-over-vsock forwarder;
- rejects every other loopback destination;
- rejects IPv4 private, link-local, carrier-grade NAT, benchmarking, multicast, limited-broadcast,
  and documentation/reserved ranges that cannot be ordinary public build destinations;
- rejects IPv6 loopback, unspecified, IPv4-mapped private, unique-local, link-local, multicast, and
  other non-global destinations;
- rejects configured controller and metadata destinations even when they are globally routed;
- allows remaining outbound TCP and UDP for ordinary CI, including the reviewed public DNS
  resolvers; and
- adds no inbound forwarding rule.

The service account is unprivileged and receives no PF mutation authority. A separately approved
root installation owns the account, PF anchor, main-ruleset attachment, LaunchDaemon, executable,
and enrollment placement. Service startup observes the active PF state and refuses new provisioning
unless the exact installed policy, service UID, executable, enrollment, fixed Lima control port,
and prepared-template network inputs match. Lifecycle polling and cleanup remain available when
admission is refused.

The initial host-wide capacity is exactly one disposable worker. Lima's per-instance gVisor network
then provides NAT separation and no peer worker exists. Increasing concurrency requires an explicit
peer-isolation design and acceptance test; this decision does not silently generalize to a shared
multi-worker subnet.

## Consequences

This boundary delegates packet filtering and VM networking to mature macOS,
Virtualization.framework, Lima, gVisor, and PF components. SmolRunner owns only their fixed
configuration, external identity, startup gate, and reconciliation. It does not implement a packet
parser, proxy, DNS server, or guest firewall attestation system.

The production installer is a privileged, high-risk boundary and must be delivered separately from
the existing user LaunchAgent apply path. Until its exact install/observe/remove and physical denial
fixtures pass, the service may run trusted development workloads but must report hostile-CI network
admission as unavailable.

Connection, rate, and byte ceilings remain later hardening. Project-specific private-network
exceptions are also deferred; they must never be supplied by workflow input.

