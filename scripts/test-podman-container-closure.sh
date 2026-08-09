#!/usr/bin/env bash
set -euo pipefail

export LC_ALL=C

podman_probe() {
  /usr/bin/podman \
    --remote=false \
    --runtime=/usr/bin/crun \
    --conmon=/usr/bin/conmon \
    --events-backend=none \
    --hooks-dir="$PROBE_HOOKS" \
    --network-config-dir="$PROBE_NETWORK" \
    --cgroup-manager=cgroupfs \
    --tmpdir="$TMPDIR" \
    --transient-store \
    "$@"
}

validate_network_lock() {
  local path=$1
  local owner mode links size
  read -r owner mode links size < <(/usr/bin/stat -Lc '%u %a %h %s' "$path")
  [[ ! -L $path ]] &&
    [[ -f $path ]] &&
    [[ $owner == "$PROBE_UID" ]] &&
    [[ $mode == 600 ]] &&
    [[ $links == 1 ]] &&
    (( size == 0 ))
}

PROBE_OWNED_CONTAINER_ID=
PROBE_OWNED_IMAGE_ID=

cleanup_user_probe() {
  local status=$?
  trap - EXIT
  set +e
  if [[ $PROBE_OWNED_CONTAINER_ID =~ ^[0-9a-f]{64}$ ]]; then
    podman_probe rm --force "$PROBE_OWNED_CONTAINER_ID" >/dev/null 2>&1
  fi
  if [[ $PROBE_OWNED_IMAGE_ID =~ ^sha256:[0-9a-f]{64}$ ]]; then
    podman_probe image rm --force "$PROBE_OWNED_IMAGE_ID" >/dev/null 2>&1
  fi
  exit "$status"
}

run_user_probe() {
  local image_id image_hex container_id container_output expected_output
  local init_output init_status start_output start_status inspect_status logs_status completion_seen=0
  local image_size container_size spec_size spec_path apparmor_profile
  local log_owner log_group log_mode log_links log_size stderr_size exit_code
  local seccomp_sha_now

  mapfile -d '' network_entries_before < <(
    /usr/bin/find "$PROBE_NETWORK" -mindepth 1 -maxdepth 1 -print0
  )
  if [[ ${#network_entries_before[@]} -ne 2 ]] ||
    ! validate_network_lock "$PROBE_NETWORK/cni.lock" ||
    ! validate_network_lock "$PROBE_NETWORK/netavark.lock"; then
    printf 'error: exact precreated network lock state was absent before image installation\n' >&2
    exit 1
  fi

  seccomp_sha_now=$(/usr/bin/sha256sum "$PROBE_SECCOMP_PROFILE" | /usr/bin/awk '{ print $1 }')
  if [[ $seccomp_sha_now != "$PROBE_SECCOMP_SHA" ]]; then
    printf 'error: exact packaged seccomp profile changed before container creation\n' >&2
    exit 1
  fi

  image_id=$(podman_probe import "$PROBE_ROOTFS_TAR" localhost/smolrunner-closure-fixture:local)
  if [[ ! $image_id =~ ^sha256:[0-9a-f]{64}$ ]]; then
    printf 'error: offline image import returned a noncanonical identity\n' >&2
    exit 1
  fi
  PROBE_OWNED_IMAGE_ID=$image_id
  image_hex=${image_id#sha256:}

  podman_probe image inspect "$image_id" > "$PROBE_IMAGE_JSON"
  image_size=$(/usr/bin/stat -Lc %s "$PROBE_IMAGE_JSON")
  if (( image_size == 0 || image_size > 1048576 )) ||
    ! /usr/bin/jq -e --arg image_hex "$image_hex" '
      length == 1 and
      .[0].Id == $image_hex and
      (.[0].RepoTags | index("localhost/smolrunner-closure-fixture:local")) != null
    ' "$PROBE_IMAGE_JSON" >/dev/null; then
    printf 'image_inspect_bytes=%s\n' "$image_size" >&2
    /usr/bin/jq -c 'if length == 1 then {id: .[0].Id, tags: .[0].RepoTags} else {count: length} end' \
      "$PROBE_IMAGE_JSON" >&2 || true
    printf 'error: offline image inspection was absent, oversized, or mismatched\n' >&2
    exit 1
  fi

  container_id=$(podman_probe create \
    --pull=never \
    --init \
    --init-path=/usr/bin/catatonit \
    --network=none \
    --no-hosts \
    --ipc=private \
    --shm-size=1048576 \
    --pid=private \
    --uts=private \
    --hostname=smolrunner-verification \
    --read-only \
    --read-only-tmpfs=false \
    --image-volume=ignore \
    --cap-drop=all \
    --security-opt=no-new-privileges \
    --security-opt="apparmor=$PROBE_APPARMOR_PROFILE" \
    --security-opt="seccomp=$PROBE_SECCOMP_PROFILE" \
    --pids-limit=32 \
    --memory=67108864 \
    --memory-swap=67108864 \
    --cpus=0.5 \
    --env-host=false \
    --http-proxy=false \
    --log-driver=k8s-file \
    --log-opt="path=$PROBE_LOGFILE" \
    --log-opt=max-size=1048576 \
    --privileged=false \
    --systemd=false \
    --restart=no \
    --no-healthcheck \
    --name=smolrunner-closure-fixture \
    --cidfile="$PROBE_CIDFILE" \
    --userns=keep-id:uid=1000,gid=1000 \
    --user=1000:1000 \
    --workdir=/ \
    --entrypoint=/bin/busybox \
    --tmpfs=/tmp:rw,noexec,nosuid,nodev,size=1048576,mode=1777 \
    "$image_id" \
    sha256sum /etc/passwd /etc/group)
  if [[ ! $container_id =~ ^[0-9a-f]{64}$ ]] ||
    [[ $(< "$PROBE_CIDFILE") != "$container_id" ]]; then
    printf 'error: stopped create returned a noncanonical container identity\n' >&2
    exit 1
  fi
  PROBE_OWNED_CONTAINER_ID=$container_id

  set +e
  /usr/bin/timeout --signal=KILL 20s \
    /usr/bin/podman \
    --remote=false \
    --runtime=/usr/bin/crun \
    --conmon=/usr/bin/conmon \
    --events-backend=none \
    --hooks-dir="$PROBE_HOOKS" \
    --network-config-dir="$PROBE_NETWORK" \
    --cgroup-manager=cgroupfs \
    --tmpdir="$TMPDIR" \
    --transient-store \
    container init "$container_id" </dev/null \
    > "$PROBE_INIT_STDOUT" 2> "$PROBE_INIT_STDERR"
  init_status=$?
  set -e
  init_output=$(< "$PROBE_INIT_STDOUT")
  stderr_size=$(/usr/bin/stat -Lc %s "$PROBE_INIT_STDERR")
  if (( init_status != 0 || stderr_size != 0 )) || [[ $init_output != "$container_id" ]]; then
    printf 'init_status=%s init_output=%q init_stderr_bytes=%s\n' \
      "$init_status" "$init_output" "$stderr_size" >&2
    printf 'error: bounded init did not return the exact stopped container identity\n' >&2
    exit 1
  fi

  podman_probe container inspect "$container_id" > "$PROBE_CONTAINER_JSON"
  container_size=$(/usr/bin/stat -Lc %s "$PROBE_CONTAINER_JSON")
  if (( container_size == 0 || container_size > 1048576 )) ||
    ! /usr/bin/jq -e \
      --arg container_id "$container_id" \
      --arg image_hex "$image_hex" \
      --arg log_path "$PROBE_LOGFILE" '
      length == 1 and
      .[0].Id == $container_id and
      .[0].Image == $image_hex and
      .[0].State.Status == "initialized" and
      .[0].Config.User == "1000:1000" and
      .[0].Config.WorkingDir == "/" and
      .[0].Config.Entrypoint == "/bin/busybox" and
      .[0].HostConfig.NetworkMode == "none" and
      .[0].HostConfig.ReadonlyRootfs == true and
      .[0].HostConfig.Privileged == false and
      .[0].HostConfig.PidsLimit == 32 and
      .[0].HostConfig.Memory == 67108864 and
      .[0].HostConfig.MemorySwap == 67108864 and
      .[0].HostConfig.LogConfig.Type == "k8s-file" and
      .[0].HostConfig.LogConfig.Path == $log_path and
      .[0].HostConfig.LogConfig.Size == "1.049MB"
    ' "$PROBE_CONTAINER_JSON" >/dev/null; then
    /usr/bin/jq -c '.[0] | {
      state: .State.Status,
      user: .Config.User,
      entrypoint: .Config.Entrypoint,
      network: .HostConfig.NetworkMode,
      readonly: .HostConfig.ReadonlyRootfs,
      privileged: .HostConfig.Privileged,
      pids: .HostConfig.PidsLimit,
      memory: .HostConfig.Memory,
      swap: .HostConfig.MemorySwap,
      log: .HostConfig.LogConfig
    }' "$PROBE_CONTAINER_JSON" >&2 || true
    printf 'error: stopped container inspection did not match the closed fixture\n' >&2
    exit 1
  fi
  if /usr/bin/grep -Fq "$PROBE_USER" "$PROBE_CONTAINER_JSON"; then
    printf 'error: stopped container inspection contains the host account name\n' >&2
    exit 1
  fi

  mapfile -d '' matching_specs < <(
    /usr/bin/find "$PROBE_ROOT" -xdev -type f -name config.json \
      -path "*$container_id*" -print0 2>/dev/null
  )
  if [[ ${#matching_specs[@]} -ne 1 ]]; then
    printf 'spec_count=%s\n' "${#matching_specs[@]}" >&2
    printf 'error: exact generated OCI specification was not uniquely available before start\n' >&2
    exit 1
  fi
  spec_path=${matching_specs[0]}
  spec_size=$(/usr/bin/stat -Lc %s "$spec_path")
  if (( spec_size == 0 || spec_size > 1048576 )) ||
    ! /usr/bin/jq -e \
      --arg apparmor_profile "$PROBE_APPARMOR_PROFILE" '
      .process.user.uid == 1000 and
      .process.user.gid == 1000 and
      .process.user.umask == 18 and
      .process.user.additionalGids == [1000] and
      .process.noNewPrivileges == true and
      (.process.capabilities | type == "object" and length == 0) and
      .process.apparmorProfile == $apparmor_profile and
      ([.mounts[] | select(.destination == "/etc/passwd" or .destination == "/etc/group")] | length) == 0 and
      .linux.seccomp != null
    ' "$spec_path" >/dev/null; then
    /usr/bin/jq -c '{
      user: .process.user,
      no_new_privileges: .process.noNewPrivileges,
      capabilities: .process.capabilities,
      account_mounts: [.mounts[] | select(.destination == "/etc/passwd" or .destination == "/etc/group") | .destination],
      seccomp_present: (.linux.seccomp != null),
      apparmor: (.process.apparmorProfile // "")
    }' "$spec_path" >&2 || true
    printf 'error: generated OCI spec weakened identity, capabilities, seccomp, or account-file ownership\n' >&2
    exit 1
  fi
  apparmor_profile=$(/usr/bin/jq -r '.process.apparmorProfile // ""' "$spec_path")
  if [[ $apparmor_profile != "$PROBE_APPARMOR_PROFILE" ]]; then
    printf 'apparmor_profile=%q\n' "$apparmor_profile" >&2
    printf 'error: generated OCI spec lacks a bounded AppArmor profile\n' >&2
    exit 1
  fi

  if /usr/bin/find "$PROBE_GRAPHROOT" "$PROBE_RUNROOT" -type f \
    \( -path "*/$container_id/userdata/passwd" -o -path "*/$container_id/userdata/group" \) \
    -print -quit 2>/dev/null | /usr/bin/grep -q .; then
    printf 'error: Podman synthesized runtime passwd/group files\n' >&2
    exit 1
  fi

  set +e
  start_output=$(/usr/bin/timeout --signal=KILL 20s \
    /usr/bin/podman \
    --remote=false \
    --runtime=/usr/bin/crun \
    --conmon=/usr/bin/conmon \
    --events-backend=none \
    --hooks-dir="$PROBE_HOOKS" \
    --network-config-dir="$PROBE_NETWORK" \
    --cgroup-manager=cgroupfs \
    --tmpdir="$TMPDIR" \
    --transient-store \
    start "$container_id" </dev/null)
  start_status=$?
  set -e
  if (( start_status != 0 )) || [[ $start_output != "$container_id" ]]; then
    podman_probe container inspect "$container_id" > "$PROBE_CONTAINER_JSON" || true
    printf 'start_status=%s start_output=%q\n' "$start_status" "$start_output" >&2
    /usr/bin/jq -c 'if length == 1 then {
      state: .[0].State.Status,
      running: .[0].State.Running,
      exit_code: .[0].State.ExitCode,
      error: .[0].State.Error,
      oom: .[0].State.OOMKilled
    } else {count: length} end' "$PROBE_CONTAINER_JSON" >&2 || true
    printf 'error: bounded detached start did not return the exact container identity\n' >&2
    exit 1
  fi

  for _ in {1..200}; do
    set +e
    /usr/bin/timeout --signal=KILL 2s \
      /usr/bin/podman \
      --remote=false \
      --runtime=/usr/bin/crun \
      --conmon=/usr/bin/conmon \
      --events-backend=none \
      --hooks-dir="$PROBE_HOOKS" \
      --network-config-dir="$PROBE_NETWORK" \
      --cgroup-manager=cgroupfs \
      --tmpdir="$TMPDIR" \
      --transient-store \
      container inspect "$container_id" </dev/null > "$PROBE_CONTAINER_JSON"
    inspect_status=$?
    set -e
    if (( inspect_status != 0 )); then
      printf 'inspect_status=%s\n' "$inspect_status" >&2
      printf 'error: bounded completion inspection failed\n' >&2
      exit 1
    fi
    container_size=$(/usr/bin/stat -Lc %s "$PROBE_CONTAINER_JSON")
    if (( container_size == 0 || container_size > 1048576 )); then
      printf 'error: bounded completion inspection was absent or oversized\n' >&2
      exit 1
    fi
    if /usr/bin/jq -e 'length == 1 and .[0].State.Status == "stopped" and .[0].State.Running == false' \
      "$PROBE_CONTAINER_JSON" >/dev/null; then
      completion_seen=1
      exit_code=$(/usr/bin/jq -r '.[0].State.ExitCode' "$PROBE_CONTAINER_JSON")
      break
    fi
    if ! /usr/bin/jq -e 'length == 1 and .[0].State.Status == "running" and .[0].State.Running == true' \
      "$PROBE_CONTAINER_JSON" >/dev/null; then
      printf 'error: completion inspection observed an unexpected container state\n' >&2
      exit 1
    fi
    /usr/bin/sleep 0.05
  done
  if (( completion_seen != 1 )) || [[ ${exit_code:-} != 0 ]]; then
    printf 'completion_seen=%s exit_code=%s\n' "$completion_seen" "${exit_code:-unknown}" >&2
    printf 'error: bounded inspection did not report one clean payload exit\n' >&2
    exit 1
  fi

  set +e
  container_output=$(/usr/bin/timeout --signal=KILL 20s \
    /usr/bin/podman \
    --remote=false \
    --runtime=/usr/bin/crun \
    --conmon=/usr/bin/conmon \
    --events-backend=none \
    --hooks-dir="$PROBE_HOOKS" \
    --network-config-dir="$PROBE_NETWORK" \
    --cgroup-manager=cgroupfs \
    --tmpdir="$TMPDIR" \
    --transient-store \
    logs "$container_id" </dev/null 2>"$PROBE_CAPTURE_STDERR")
  logs_status=$?
  set -e
  stderr_size=$(/usr/bin/stat -Lc %s "$PROBE_CAPTURE_STDERR")
  if (( logs_status != 0 || stderr_size != 0 )); then
    printf 'logs_status=%s logs_stderr_bytes=%s\n' "$logs_status" "$stderr_size" >&2
    printf 'error: bounded log retrieval did not report one clean payload exit\n' >&2
    exit 1
  fi

  expected_output="$PROBE_PASSWD_SHA  /etc/passwd
$PROBE_GROUP_SHA  /etc/group"
  if [[ $container_output != "$expected_output" ]]; then
    printf 'error: in-container account files differ from exact image-owned bytes\n' >&2
    exit 1
  fi

  read -r log_owner log_group log_mode log_links log_size < <(
    /usr/bin/stat -Lc '%u %g %a %h %s' "$PROBE_LOGFILE"
  )
  if [[ -L $PROBE_LOGFILE ]] || [[ ! -f $PROBE_LOGFILE ]] ||
    [[ $log_owner != "$PROBE_UID" ]] || [[ $log_group != "$PROBE_GID" ]] ||
    (( (8#$log_mode & 0022) != 0 )) || [[ $log_links != 1 ]] ||
    (( log_size == 0 || log_size >= 1048576 )); then
    printf 'error: payload log was absent, unsafe, or reached its exact overflow boundary\n' >&2
    exit 1
  fi

  podman_probe container inspect "$container_id" > "$PROBE_CONTAINER_JSON"
  if ! /usr/bin/jq -e 'length == 1 and .[0].State.Status == "stopped" and .[0].State.ExitCode == 0' \
    "$PROBE_CONTAINER_JSON" >/dev/null; then
    printf 'error: trusted account gate did not exit successfully\n' >&2
    exit 1
  fi

  podman_probe rm "$container_id" >/dev/null
  if podman_probe container exists "$container_id"; then
    printf 'error: exact fixture container still exists after removal\n' >&2
    exit 1
  fi
  PROBE_OWNED_CONTAINER_ID=
  podman_probe image rm "$image_id" >/dev/null
  PROBE_OWNED_IMAGE_ID=

  mapfile -d '' network_entries_after < <(
    /usr/bin/find "$PROBE_NETWORK" -mindepth 1 -maxdepth 1 -print0
  )
  if [[ ${#network_entries_after[@]} -ne 2 ]] ||
    ! validate_network_lock "$PROBE_NETWORK/cni.lock" ||
    ! validate_network_lock "$PROBE_NETWORK/netavark.lock"; then
    printf 'error: offline container attempt created unexpected network state\n' >&2
    exit 1
  fi

  printf 'offline_image=exact stopped_create=closed account_files=image-owned apparmor=%s\n' \
    "$apparmor_profile"
}

if [[ ${1:-} == --user-probe ]]; then
  trap cleanup_user_probe EXIT
  run_user_probe
  exit 0
fi

if [[ $EUID -ne 0 ]]; then
  printf 'error: disposable container probe must run as uid 0\n' >&2
  exit 1
fi
if [[ ${SMOLRUNNER_DISPOSABLE_PROBE:-} != github-hosted-ubuntu ]]; then
  printf 'error: disposable container probe requires the explicit hosted-CI gate\n' >&2
  exit 1
fi

for command in \
  /usr/bin/busybox \
  /usr/bin/findmnt \
  /usr/bin/jq \
  /usr/bin/podman \
  /usr/bin/sha256sum \
  /usr/bin/stat \
  /usr/bin/systemctl \
  /usr/bin/systemd-run \
  /usr/bin/tar \
  /usr/bin/timeout; do
  if [[ ! -x $command ]]; then
    printf 'error: required executable is absent: %s\n' "$command" >&2
    exit 1
  fi
done

probe_apparmor_profile=docker-default
probe_apparmor_profiles=/sys/kernel/security/apparmor/profiles
if [[ ! -r $probe_apparmor_profiles ]] ||
  ! /usr/bin/grep -Fxq "$probe_apparmor_profile (enforce)" "$probe_apparmor_profiles"; then
  printf 'error: exact disposable AppArmor profile is not loaded in enforce mode\n' >&2
  exit 1
fi

probe_seccomp_source=/usr/share/containers/seccomp.json
probe_seccomp_expected_sha=cc374cf23846ce1f62f4dc807a8e2b8673c783c6f56cb475467621035d281e6c
if [[ -L $probe_seccomp_source ]] || [[ ! -f $probe_seccomp_source ]]; then
  printf 'error: exact packaged seccomp profile is absent or not a regular file\n' >&2
  exit 1
fi
seccomp_source_sha=$(/usr/bin/sha256sum "$probe_seccomp_source" | /usr/bin/awk '{ print $1 }')
read -r seccomp_owner seccomp_group seccomp_links seccomp_size < <(
  /usr/bin/stat -Lc '%u %g %h %s' "$probe_seccomp_source"
)
if [[ $seccomp_owner != 0 ]] || [[ $seccomp_group != 0 ]] || [[ $seccomp_links != 1 ]] ||
  (( seccomp_size == 0 || seccomp_size > 1048576 )) ||
  [[ $seccomp_source_sha != "$probe_seccomp_expected_sha" ]]; then
  printf 'seccomp_owner=%s seccomp_group=%s seccomp_links=%s seccomp_size=%s seccomp_sha256=%s\n' \
    "$seccomp_owner" "$seccomp_group" "$seccomp_links" "$seccomp_size" \
    "$seccomp_source_sha" >&2
  printf 'error: packaged seccomp source does not match the pinned disposable fixture\n' >&2
  exit 1
fi

case "$(/usr/bin/uname -m)" in
  aarch64 | x86_64) ;;
  *)
    printf 'error: unsupported probe architecture\n' >&2
    exit 1
    ;;
esac

probe_root=$(/usr/bin/mktemp -d /tmp/smolrunner-podman-container.XXXXXX)
probe_nonce=${probe_root##*.}
probe_nonce=${probe_nonce,,}
probe_user="smolctr_$probe_nonce"
probe_unit="smolrunner-podman-container-$probe_nonce.service"
probe_user_created=0
probe_unit_owned=0
probe_script=$(/usr/bin/readlink -f "$0")
probe_user_script="$probe_root/user-probe.sh"
probe_seccomp_profile="$probe_root/seccomp.json"

cleanup() {
  local status=$?
  set +e
  if [[ $probe_unit_owned -eq 1 ]]; then
    /usr/bin/systemctl stop "$probe_unit" >/dev/null 2>&1
  fi
  if [[ $probe_user_created -eq 1 ]]; then
    /usr/sbin/userdel "$probe_user" >/dev/null 2>&1
  fi
  case "$probe_root" in
    /tmp/smolrunner-podman-container.*) /usr/bin/rm -rf -- "$probe_root" ;;
  esac
  exit "$status"
}
trap cleanup EXIT

if /usr/bin/id "$probe_user" >/dev/null 2>&1; then
  printf 'error: unique disposable probe user already exists\n' >&2
  exit 1
fi
if ! unit_load_state=$(/usr/bin/systemctl show --property=LoadState --value "$probe_unit" 2>/dev/null) ||
  [[ $unit_load_state != not-found ]]; then
  printf 'error: unique disposable probe unit is not proven absent\n' >&2
  exit 1
fi
for mounts_conf in /usr/share/containers/mounts.conf /etc/containers/mounts.conf; do
  if [[ -e $mounts_conf ]] || [[ -L $mounts_conf ]]; then
    if [[ -L $mounts_conf ]] || [[ ! -f $mounts_conf ]] ||
      /usr/bin/awk '!/^[[:space:]]*($|#)/ { found = 1 } END { exit !found }' "$mounts_conf"; then
      printf 'error: ambient system mounts configuration is present\n' >&2
      exit 1
    fi
  fi
done

/usr/sbin/useradd --home-dir "$probe_root/empty-home" --no-create-home --shell /bin/bash "$probe_user"
probe_user_created=1
probe_uid=$(/usr/bin/id -u "$probe_user")
probe_gid=$(/usr/bin/id -g "$probe_user")
if [[ $(/usr/bin/awk -F: -v user="$probe_user" '$1 == user { count += 1 } END { print count + 0 }' /etc/subuid) != 1 ]] ||
  [[ $(/usr/bin/awk -F: -v user="$probe_user" '$1 == user { count += 1 } END { print count + 0 }' /etc/subgid) != 1 ]]; then
  printf 'error: probe user lacks one exact subordinate ID authority row\n' >&2
  exit 1
fi

mkdir -p \
  "$probe_root/auth" \
  "$probe_root/config" \
  "$probe_root/empty-home" \
  "$probe_root/empty-xdg" \
  "$probe_root/graphroot" \
  "$probe_root/hooks" \
  "$probe_root/network" \
  "$probe_root/rootfs/bin" \
  "$probe_root/rootfs/etc" \
  "$probe_root/rootfs/tmp" \
  "$probe_root/runtime" \
  "$probe_root/tmp"
chmod 0755 "$probe_root"
install -o 0 -g 0 -m 0555 "$probe_script" "$probe_user_script"
install -o 0 -g 0 -m 0555 /usr/bin/busybox "$probe_root/rootfs/bin/busybox"
install -o 0 -g 0 -m 0444 "$probe_seccomp_source" "$probe_seccomp_profile"
seccomp_sha=$(/usr/bin/sha256sum "$probe_seccomp_profile" | /usr/bin/awk '{ print $1 }')
read -r seccomp_owner seccomp_group seccomp_mode seccomp_links seccomp_size < <(
  /usr/bin/stat -Lc '%u %g %a %h %s' "$probe_seccomp_profile"
)
if [[ -L $probe_seccomp_profile ]] || [[ ! -f $probe_seccomp_profile ]] ||
  [[ $seccomp_owner != 0 ]] || [[ $seccomp_group != 0 ]] || [[ $seccomp_mode != 444 ]] ||
  [[ $seccomp_links != 1 ]] || [[ $seccomp_sha != "$probe_seccomp_expected_sha" ]] ||
  (( seccomp_size == 0 || seccomp_size > 1048576 )); then
  printf 'error: attempt-private seccomp snapshot is not exact and immutable\n' >&2
  exit 1
fi
printf 'root:x:0:0:root:/root:/bin/busybox\nsmolgate:x:1000:1000:SmolRunner gate:/:/bin/busybox\n' \
  > "$probe_root/rootfs/etc/passwd"
printf 'root:x:0:\nsmolgate:x:1000:\n' > "$probe_root/rootfs/etc/group"
chmod 0444 "$probe_root/rootfs/etc/passwd" "$probe_root/rootfs/etc/group"
chmod 1777 "$probe_root/rootfs/tmp"

passwd_sha=$(/usr/bin/sha256sum "$probe_root/rootfs/etc/passwd" | /usr/bin/awk '{ print $1 }')
group_sha=$(/usr/bin/sha256sum "$probe_root/rootfs/etc/group" | /usr/bin/awk '{ print $1 }')
/usr/bin/tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner \
  --format=posix -C "$probe_root/rootfs" -cf "$probe_root/rootfs.tar" .
chmod 0444 "$probe_root/rootfs.tar"

printf '' > "$probe_root/config/containers.conf"
printf 'unqualified-search-registries = []\nshort-name-mode = "enforcing"\n' \
  > "$probe_root/config/registries.conf"
printf '[storage]\ndriver = "overlay"\nrunroot = "%s"\ngraphroot = "%s"\nrootless_storage_path = "%s"\n\n[storage.options]\nadditionalimagestores = []\n' \
  "$probe_root/runtime/containers" "$probe_root/graphroot" "$probe_root/graphroot" \
  > "$probe_root/config/storage.conf"
printf '{}\n' > "$probe_root/auth/auth.json"
printf '' > "$probe_root/network/cni.lock"
printf '' > "$probe_root/network/netavark.lock"

chown -R "$probe_uid:$probe_gid" \
  "$probe_root/graphroot" \
  "$probe_root/runtime" \
  "$probe_root/tmp"
chown "$probe_uid:$probe_gid" \
  "$probe_root/network/cni.lock" \
  "$probe_root/network/netavark.lock"
chmod 0555 "$probe_root/empty-home" "$probe_root/empty-xdg"
chmod 0700 "$probe_root/graphroot" "$probe_root/runtime" "$probe_root/tmp"
chmod 0600 "$probe_root/network/cni.lock" "$probe_root/network/netavark.lock"
chmod 0555 "$probe_root/network"
chmod -R a-w "$probe_root/auth" "$probe_root/config" "$probe_root/hooks" "$probe_root/rootfs"

probe_unit_owned=1
/usr/bin/systemd-run \
  --wait \
  --collect \
  --pipe \
  --service-type=exec \
  --unit="${probe_unit%.service}" \
  --uid="$probe_user" \
  --property=Delegate=yes \
  --property=KillMode=control-group \
  /usr/bin/env -i \
  CONTAINERS_CONF="$probe_root/config/containers.conf" \
  CONTAINERS_REGISTRIES_CONF="$probe_root/config/registries.conf" \
  CONTAINERS_STORAGE_CONF="$probe_root/config/storage.conf" \
  DBUS_SESSION_BUS_ADDRESS="unix:path=$probe_root/runtime/absent-user-bus" \
  HOME="$probe_root/empty-home" \
  LC_ALL=C \
  LOGNAME="$probe_user" \
  PATH=/usr/bin \
  PROBE_APPARMOR_PROFILE="$probe_apparmor_profile" \
  PROBE_CIDFILE="$probe_root/runtime/container.cid" \
  PROBE_CAPTURE_STDERR="$probe_root/runtime/logs.stderr" \
  PROBE_CONTAINER_JSON="$probe_root/runtime/container.json" \
  PROBE_GRAPHROOT="$probe_root/graphroot" \
  PROBE_GROUP_SHA="$group_sha" \
  PROBE_HOOKS="$probe_root/hooks" \
  PROBE_IMAGE_JSON="$probe_root/runtime/image.json" \
  PROBE_INIT_STDERR="$probe_root/runtime/init.stderr" \
  PROBE_INIT_STDOUT="$probe_root/runtime/init.stdout" \
  PROBE_LOGFILE="$probe_root/runtime/container.log" \
  PROBE_NETWORK="$probe_root/network" \
  PROBE_PASSWD_SHA="$passwd_sha" \
  PROBE_ROOT="$probe_root" \
  PROBE_ROOTFS_TAR="$probe_root/rootfs.tar" \
  PROBE_RUNROOT="$probe_root/runtime/containers" \
  PROBE_SECCOMP_PROFILE="$probe_seccomp_profile" \
  PROBE_SECCOMP_SHA="$seccomp_sha" \
  PROBE_GID="$probe_gid" \
  PROBE_UID="$probe_uid" \
  PROBE_USER="$probe_user" \
  REGISTRY_AUTH_FILE="$probe_root/auth/auth.json" \
  TMPDIR="$probe_root/tmp" \
  USER="$probe_user" \
  XDG_CONFIG_HOME="$probe_root/empty-xdg" \
  XDG_RUNTIME_DIR="$probe_root/runtime" \
  /usr/bin/bash "$probe_user_script" --user-probe
probe_unit_owned=0

if /usr/bin/pgrep -u "$probe_uid" >/dev/null 2>&1; then
  printf 'error: offline container probe left an idle process\n' >&2
  exit 1
fi
if /usr/bin/findmnt -rn -o TARGET | /usr/bin/grep -Fq "$probe_root"; then
  printf 'error: offline container probe left a mount below its run-private root\n' >&2
  exit 1
fi

printf 'podman_container_closure_probe=pass\n'
