#!/usr/bin/env bash
set -euo pipefail

export LC_ALL=C

if [[ $EUID -ne 0 ]]; then
  printf 'error: disposable package probe must run as uid 0\n' >&2
  exit 1
fi

for command in \
  /usr/bin/dpkg-query \
  /usr/bin/findmnt \
  /usr/bin/git \
  /usr/bin/jq \
  /usr/bin/mount \
  /usr/bin/podman \
  /usr/bin/python3 \
  /usr/bin/readlink \
  /usr/sbin/runuser \
  /usr/bin/stat \
  /usr/bin/systemd-run \
  /usr/bin/umount; do
  if [[ ! -x $command ]]; then
    printf 'error: required executable is absent: %s\n' "$command" >&2
    exit 1
  fi
done

case "$(/usr/bin/uname -m)" in
  aarch64 | x86_64) ;;
  *)
    printf 'error: unsupported probe architecture\n' >&2
    exit 1
    ;;
esac

probe_root=$(/usr/bin/mktemp -d /tmp/smolrunner-podman-closure.XXXXXX)
probe_user=smolprobe
probe_unit=smolrunner-podman-closure-probe.service
target_mount="$probe_root/target-tmpfs"
target_mounted=0

cleanup() {
  local status=$?
  set +e
  /usr/bin/systemctl stop "$probe_unit" >/dev/null 2>&1
  if [[ $target_mounted -eq 1 ]] && /usr/bin/findmnt -rn --target "$target_mount" >/dev/null 2>&1; then
    /usr/bin/umount "$target_mount"
  fi
  if /usr/bin/id "$probe_user" >/dev/null 2>&1; then
    /usr/sbin/userdel "$probe_user" >/dev/null 2>&1
  fi
  case "$probe_root" in
    /tmp/smolrunner-podman-closure.*) /usr/bin/rm -rf -- "$probe_root" ;;
  esac
  exit "$status"
}
trap cleanup EXIT

assert_root_file() {
  local path=$1
  local owner group mode kind links
  if [[ -L $path ]]; then
    printf 'error: package executable path is a symlink: %s\n' "$path" >&2
    exit 1
  fi
  read -r owner group mode links kind < <(/usr/bin/stat -Lc '%u %g %a %h %F' "$path")
  if [[ $owner != 0 || $group != 0 || $kind != 'regular file' ]]; then
    printf 'error: package file metadata is unsafe: %s\n' "$path" >&2
    exit 1
  fi
  if (( (8#$mode & 0022) != 0 )); then
    printf 'error: package file is group/world writable: %s\n' "$path" >&2
    exit 1
  fi
  if ((links != 1)); then
    printf 'error: package file is not single-link: %s\n' "$path" >&2
    exit 1
  fi
}

printf 'probe_architecture=%s\n' "$(/usr/bin/uname -m)"
/usr/bin/dpkg-query -W -f='package=${Package} version=${Version}\n' \
  aardvark-dns busybox-static catatonit conmon crun fuse-overlayfs git netavark podman uidmap

podman_version=$(/usr/bin/podman --version)
git_version=$(/usr/bin/git --version)
case "$podman_version" in
  'podman version 4.9.3'*) ;;
  *)
    printf 'error: unexpected Podman baseline: %s\n' "$podman_version" >&2
    exit 1
    ;;
esac
case "$git_version" in
  'git version 2.43.'*) ;;
  *)
    printf 'error: unexpected Git baseline: %s\n' "$git_version" >&2
    exit 1
    ;;
esac

for path in \
  /usr/bin/podman \
  /usr/bin/git \
  /usr/bin/conmon \
  /usr/bin/crun \
  /usr/bin/newuidmap \
  /usr/bin/newgidmap \
  /usr/bin/catatonit \
  /usr/bin/fuse-overlayfs \
  /usr/lib/podman/aardvark-dns \
  /usr/lib/podman/netavark; do
  assert_root_file "$path"
done

if [[ ! -L /usr/libexec/podman/catatonit ]] ||
  [[ $(/usr/bin/readlink -f /usr/libexec/podman/catatonit) != /usr/bin/catatonit ]]; then
  printf 'error: packaged Podman catatonit fallback is not the exact expected symlink\n' >&2
  exit 1
fi

for helper in /usr/bin/newuidmap /usr/bin/newgidmap; do
  helper_mode=$(/usr/bin/stat -Lc %a "$helper")
  if (( (8#$helper_mode & 04000) == 0 )); then
    printf 'error: namespace helper is not setuid: %s\n' "$helper" >&2
    exit 1
  fi
done

if [[ -e /etc/containers/podman_preexec_hooks.txt ]]; then
  printf 'error: Podman pre-exec hook indicator is present\n' >&2
  exit 1
fi
for hook_directory in /usr/libexec/podman/pre-exec-hooks /etc/containers/pre-exec-hooks; do
  if [[ -d $hook_directory ]] &&
    [[ -n $(/usr/bin/find "$hook_directory" -mindepth 1 -maxdepth 1 -print -quit) ]]; then
    printf 'error: Podman pre-exec hook directory is not empty\n' >&2
    exit 1
  fi
done

/usr/sbin/useradd --create-home --home-dir "$probe_root/hostile-home" --shell /bin/bash "$probe_user"
probe_uid=$(/usr/bin/id -u "$probe_user")
probe_gid=$(/usr/bin/id -g "$probe_user")

if [[ $(/usr/bin/awk -F: -v user="$probe_user" '$1 == user { count += 1 } END { print count + 0 }' /etc/subuid) != 1 ]] ||
  [[ $(/usr/bin/awk -F: -v user="$probe_user" '$1 == user { count += 1 } END { print count + 0 }' /etc/subgid) != 1 ]]; then
  printf 'error: probe user lacks one exact subordinate ID authority row\n' >&2
  exit 1
fi

mkdir -p \
  "$probe_root/empty-home" \
  "$probe_root/empty-xdg" \
  "$probe_root/git-source" \
  "$probe_root/git-synthetic/objects" \
  "$probe_root/git-synthetic/refs/heads" \
  "$probe_root/git-synthetic/refs/tags" \
  "$probe_root/git-synthetic-other/objects" \
  "$probe_root/git-synthetic-other/refs/heads" \
  "$probe_root/git-synthetic-other/refs/tags" \
  "$target_mount"
chmod 0755 "$probe_root" "$probe_root/empty-home" "$probe_root/empty-xdg"
chown -R "$probe_uid:$probe_gid" "$probe_root/git-source"

mkdir -p "$probe_root/hostile-home/.config/containers"
printf 'this is deliberately invalid persistent runner configuration\n' \
  > "$probe_root/hostile-home/.config/containers/containers.conf"
chown -R "$probe_uid:$probe_gid" "$probe_root/hostile-home"

/usr/sbin/runuser -u "$probe_user" -- /usr/bin/env -i \
  HOME="$probe_root/hostile-home" \
  LC_ALL=C \
  PATH=/usr/bin \
  /usr/bin/git init --quiet "$probe_root/git-source"
printf 'smolrunner closure probe\n' > "$probe_root/git-source/input.txt"
chown "$probe_uid:$probe_gid" "$probe_root/git-source/input.txt"
/usr/sbin/runuser -u "$probe_user" -- /usr/bin/env -i \
  HOME="$probe_root/hostile-home" \
  LC_ALL=C \
  PATH=/usr/bin \
  /usr/bin/git -C "$probe_root/git-source" -c user.name=probe -c user.email=probe.invalid \
  add input.txt
/usr/sbin/runuser -u "$probe_user" -- /usr/bin/env -i \
  HOME="$probe_root/hostile-home" \
  LC_ALL=C \
  PATH=/usr/bin \
  /usr/bin/git -C "$probe_root/git-source" -c user.name=probe -c user.email=probe.invalid \
  commit --quiet -m probe
probe_tree=$(/usr/sbin/runuser -u "$probe_user" -- /usr/bin/env -i \
  HOME="$probe_root/hostile-home" LC_ALL=C PATH=/usr/bin \
  /usr/bin/git -C "$probe_root/git-source" rev-parse 'HEAD^{tree}')

for synthetic in "$probe_root/git-synthetic" "$probe_root/git-synthetic-other"; do
  printf '[core]\n\trepositoryformatversion = 0\n\tbare = true\n' > "$synthetic/config"
  printf 'ref: refs/heads/main\n' > "$synthetic/HEAD"
  chmod -R a-w "$synthetic"
done

/usr/bin/python3 - \
  "$probe_uid" \
  "$probe_gid" \
  "$probe_root/git-source/.git/objects" \
  "$probe_root/git-synthetic" \
  "$probe_root/git-synthetic-other" \
  "$probe_tree" <<'PY'
import hashlib
import os
import subprocess
import sys

uid = int(sys.argv[1])
gid = int(sys.argv[2])
objects = sys.argv[3]
synthetic = sys.argv[4]
other = sys.argv[5]
tree = sys.argv[6]

fd = os.open(objects, os.O_RDONLY | os.O_DIRECTORY)

def demote():
    os.setgroups([])
    os.setgid(gid)
    os.setuid(uid)

environment = {
    "GIT_ALLOW_PROTOCOL": "",
    "GIT_ATTR_NOSYSTEM": "1",
    "GIT_CONFIG_GLOBAL": "/dev/null",
    "GIT_CONFIG_NOSYSTEM": "1",
    "GIT_NO_LAZY_FETCH": "1",
    "GIT_NO_REPLACE_OBJECTS": "1",
    "GIT_OBJECT_DIRECTORY": f"/proc/self/fd/{fd}",
    "GIT_OPTIONAL_LOCKS": "0",
    "GIT_TERMINAL_PROMPT": "0",
    "LC_ALL": "C",
}

def run(arguments):
    return subprocess.run(
        arguments,
        input=(tree + "\n").encode("ascii"),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=environment,
        pass_fds=(fd,),
        preexec_fn=demote,
        timeout=5,
        check=False,
    )

unsafe = run(["/usr/bin/git", f"--git-dir={synthetic}", "cat-file", "--batch"])
if unsafe.returncode == 0 or b"dubious ownership" not in unsafe.stderr:
    raise SystemExit("root-owned synthetic Git directory was not refused without safe.directory")

safe = run([
    "/usr/bin/git",
    "-c",
    f"safe.directory={synthetic}",
    f"--git-dir={synthetic}",
    "cat-file",
    "--batch",
])
if safe.returncode != 0:
    raise SystemExit("exact command-scoped safe.directory did not admit the synthetic Git directory")
header, payload = safe.stdout.split(b"\n", 1)
object_name, object_type, object_size = header.decode("ascii").split(" ")
size = int(object_size)
content = payload[:size]
if object_name != tree or object_type != "tree" or payload[size:] != b"\n":
    raise SystemExit("cat-file batch returned a noncanonical tree record")
digest = hashlib.sha1(b"tree " + str(size).encode("ascii") + b"\0" + content).hexdigest()
if digest != tree:
    raise SystemExit("materializer tree object digest mismatch")

drift = run([
    "/usr/bin/git",
    "-c",
    f"safe.directory={synthetic}",
    f"--git-dir={other}",
    "cat-file",
    "--batch",
])
if drift.returncode == 0 or b"dubious ownership" not in drift.stderr:
    raise SystemExit("safe.directory admitted a different synthetic Git path")

print("git_materializer=closed")
PY

/usr/bin/mount -t tmpfs \
  -o "size=1048576,nr_inodes=64,uid=$probe_uid,gid=$probe_gid,mode=0700,nosuid,nodev" \
  smolrunner-probe-target "$target_mount"
target_mounted=1
target_type=$(/usr/bin/findmnt -rn -o FSTYPE --target "$target_mount")
target_options=$(/usr/bin/findmnt -rn -o OPTIONS --target "$target_mount")
target_blocks=$(/usr/bin/stat -f -c %b "$target_mount")
target_block_size=$(/usr/bin/stat -f -c %S "$target_mount")
target_inodes=$(/usr/bin/stat -f -c %c "$target_mount")
if [[ $target_type != tmpfs ]] ||
  [[ $target_options != *nosuid* ]] ||
  [[ $target_options != *nodev* ]] ||
  (( target_blocks * target_block_size > 1048576 )) ||
  (( target_inodes > 64 )); then
  printf 'error: target tmpfs did not retain its hard bounds\n' >&2
  exit 1
fi
printf 'target_tmpfs=bounded blocks=%s block_size=%s inodes=%s\n' \
  "$target_blocks" "$target_block_size" "$target_inodes"
/usr/bin/umount "$target_mount"
target_mounted=0

global_help=$(/usr/bin/env -i HOME="$probe_root/empty-home" LC_ALL=C PATH=/usr/bin \
  XDG_CONFIG_HOME="$probe_root/empty-xdg" /usr/bin/podman --help)
create_help=$(/usr/bin/env -i HOME="$probe_root/empty-home" LC_ALL=C PATH=/usr/bin \
  XDG_CONFIG_HOME="$probe_root/empty-xdg" /usr/bin/podman create --help)
start_help=$(/usr/bin/env -i HOME="$probe_root/empty-home" LC_ALL=C PATH=/usr/bin \
  XDG_CONFIG_HOME="$probe_root/empty-xdg" /usr/bin/podman start --help)
for option in --cgroup-manager --conmon --events-backend --hooks-dir --network-config-dir --remote --runtime --tmpdir --transient-store; do
  if [[ $global_help != *"$option"* ]]; then
    printf 'error: Podman global option is absent: %s\n' "$option" >&2
    exit 1
  fi
done
for option in --cgroup-parent --env-host --http-proxy --mount --network --no-hosts --passwd --read-only-tmpfs --tmpfs; do
  if [[ $create_help != *"$option"* ]]; then
    printf 'error: Podman create option is absent: %s\n' "$option" >&2
    exit 1
  fi
done
if [[ $start_help != *'--attach'* ]]; then
  printf 'error: Podman start boolean attach option is absent\n' >&2
  exit 1
fi
printf 'podman_cli_surface=expected\n'

mkdir -p \
  "$probe_root/auth" \
  "$probe_root/config" \
  "$probe_root/graphroot" \
  "$probe_root/hooks" \
  "$probe_root/network" \
  "$probe_root/runroot" \
  "$probe_root/runtime" \
  "$probe_root/tmp"
chown -R "$probe_uid:$probe_gid" \
  "$probe_root/graphroot" \
  "$probe_root/runroot" \
  "$probe_root/runtime" \
  "$probe_root/tmp"
chmod 0700 \
  "$probe_root/graphroot" \
  "$probe_root/runroot" \
  "$probe_root/runtime" \
  "$probe_root/tmp"
printf '' > "$probe_root/config/containers.conf"
printf 'unqualified-search-registries = []\nshort-name-mode = "enforcing"\n' \
  > "$probe_root/config/registries.conf"
printf '[storage]\ndriver = "overlay"\nrunroot = "%s"\ngraphroot = "%s"\n\n[storage.options]\nadditionalimagestores = []\n' \
  "$probe_root/runroot" "$probe_root/graphroot" > "$probe_root/config/storage.conf"
printf '{}\n' > "$probe_root/auth/auth.json"
chmod -R a-w "$probe_root/auth" "$probe_root/config" "$probe_root/hooks" "$probe_root/network"

cat > "$probe_root/user-probe.sh" <<'USER_PROBE'
#!/usr/bin/env bash
set -euo pipefail

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
  info --format json > "$PROBE_INFO"

if ! /usr/bin/jq -e '.host.security.rootless == true' "$PROBE_INFO" >/dev/null; then
  printf 'error: Podman did not report rootless operation\n' >&2
  exit 1
fi

pause_file=$(/usr/bin/find "$XDG_RUNTIME_DIR" -type f -name pause.pid -print -quit)
if [[ -z $pause_file ]]; then
  printf 'rootless_info=success pause=not-created-by-info\n'
  exit 0
fi
pause_pid=$(/usr/bin/tr -d '\n' < "$pause_file")
if [[ ! $pause_pid =~ ^[1-9][0-9]*$ ]] || [[ ! -r /proc/$pause_pid/status ]]; then
  printf 'error: pause PID evidence is invalid\n' >&2
  exit 1
fi
pause_uid=$(/usr/bin/awk '/^Uid:/ { print $2 }' "/proc/$pause_pid/status")
if [[ $pause_uid != "$PROBE_UID" ]] ||
  ! /usr/bin/grep -Fq "/$PROBE_UNIT" "/proc/$pause_pid/cgroup"; then
  printf 'error: rootless pause process escaped the disposable service cgroup\n' >&2
  exit 1
fi
printf 'rootless_info=success pause=contained\n'
USER_PROBE
chmod 0555 "$probe_root/user-probe.sh"

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
  PROBE_HOOKS="$probe_root/hooks" \
  PROBE_INFO="$probe_root/runtime/info.json" \
  PROBE_NETWORK="$probe_root/network" \
  PROBE_UID="$probe_uid" \
  PROBE_UNIT="$probe_unit" \
  REGISTRY_AUTH_FILE="$probe_root/auth/auth.json" \
  TMPDIR="$probe_root/tmp" \
  USER="$probe_user" \
  XDG_CONFIG_HOME="$probe_root/empty-xdg" \
  XDG_RUNTIME_DIR="$probe_root/runtime" \
  /usr/bin/bash "$probe_root/user-probe.sh"

if /usr/bin/pgrep -u "$probe_uid" >/dev/null 2>&1; then
  printf 'error: rootless first-use probe left an idle process\n' >&2
  exit 1
fi
if /usr/bin/findmnt -rn -o TARGET | /usr/bin/grep -Fq "$probe_root"; then
  printf 'error: rootless first-use probe left a mount below its run-private root\n' >&2
  exit 1
fi

printf 'podman_closure_package_probe=pass\n'
