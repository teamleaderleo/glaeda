#!/usr/bin/env python3
"""Ultra-trusted native Apple builds with project-private, toolchain-keyed state."""

from __future__ import annotations

import argparse
import contextlib
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import signal
import stat
import subprocess
import sys
import time
import uuid


LIMIT = 65536
ENV_NAMES = ("HOME", "USER", "LOGNAME", "PATH", "TMPDIR", "LANG", "LC_ALL", "LC_CTYPE")
CACHE_NAMES = ("derived_data", "source_packages", "scratch", "module_cache", "products", "extensions")
XCODE_SETTINGS = {
    "ARCHS", "ONLY_ACTIVE_ARCH", "CODE_SIGNING_ALLOWED", "CODE_SIGNING_REQUIRED", "CODE_SIGN_IDENTITY",
    "DEVELOPMENT_TEAM", "CODE_SIGN_STYLE", "PRODUCT_BUNDLE_IDENTIFIER", "MACOSX_DEPLOYMENT_TARGET",
    "IPHONEOS_DEPLOYMENT_TARGET", "TVOS_DEPLOYMENT_TARGET", "WATCHOS_DEPLOYMENT_TARGET", "XROS_DEPLOYMENT_TARGET",
    "SWIFT_VERSION", "SWIFT_OPTIMIZATION_LEVEL", "GCC_OPTIMIZATION_LEVEL", "ENABLE_TESTABILITY",
    "SWIFT_ACTIVE_COMPILATION_CONDITIONS", "DEBUG_INFORMATION_FORMAT",
}


class Refusal(Exception):
    pass


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def bounded_json(raw):
    if len(raw) > LIMIT:
        raise Refusal("JSON document exceeds the size limit")
    try:
        return json.loads(raw)
    except (ValueError, UnicodeError) as error:
        raise Refusal("invalid JSON document") from error


def base_environment():
    return {key: os.environ[key] for key in ENV_NAMES if key in os.environ}


def probe(argv, environment):
    result = subprocess.run(argv, env=environment, capture_output=True, timeout=30, check=False)
    if result.returncode or len(result.stdout) > LIMIT:
        raise Refusal("Apple toolchain or project probe failed")
    return result.stdout.decode().strip()


def apple_toolchain(environment, sdk="macosx"):
    if sys.platform != "darwin":
        raise Refusal("native Apple builds require macOS")
    developer = Path(os.environ.get("DEVELOPER_DIR") or probe(["/usr/bin/xcode-select", "-p"], environment)).resolve(strict=True)
    environment = {**environment, "DEVELOPER_DIR": str(developer)}
    swift = Path(probe(["/usr/bin/xcrun", "--find", "swift"], environment)).resolve(strict=True)
    xcodebuild = developer / "usr/bin/xcodebuild"
    return {
        "developer": str(developer), "swift": str(swift), "xcodebuild": str(xcodebuild),
        "swift_version": probe([str(swift), "--version"], environment),
        "xcode_version": probe([str(xcodebuild), "-version"], environment),
        "sdk": probe(["/usr/bin/xcrun", "--sdk", sdk, "--show-sdk-path"], environment),
        "sdk_build": probe(["/usr/bin/xcrun", "--sdk", sdk, "--show-sdk-build-version"], environment),
        "host": platform.machine(),
    }


def relative_path(value):
    if not isinstance(value, str) or not value or "\x00" in value:
        raise Refusal("expected a relative project path")
    path = Path(value)
    if path.is_absolute() or any(part in ("", ".", "..") for part in value.split("/")):
        raise Refusal("project paths must be normalized and relative")
    return path


def project_file(project, value):
    path = project / relative_path(value)
    resolved = path.resolve(strict=True)
    if not resolved.is_relative_to(project):
        raise Refusal("project file escapes the checkout")
    return resolved


def configuration(project, name):
    path = project_file(project, "glaeda.apple.json")
    with path.open("rb") as stream:
        config = bounded_json(stream.read(LIMIT + 1))
    if not isinstance(config, dict) or set(config) != {"schema_version", "profiles"} or config["schema_version"] != 1:
        raise Refusal("expected Apple build configuration schema 1")
    profiles = config["profiles"]
    if not isinstance(profiles, dict) or name not in profiles or not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,63}", name):
        raise Refusal("unknown or invalid build profile")
    profile = profiles[name]
    common = {"engine", "environment", "sdk"}
    allowed = {
        "swiftpm": common | {"configuration", "product", "action", "triple", "jobs", "package"},
        "xcode": common | {"project", "workspace", "scheme", "configuration", "destination", "settings", "action"},
        "script": common | {"executable", "arguments"},
    }
    if not isinstance(profile, dict) or profile.get("engine") not in allowed or set(profile) - allowed[profile["engine"]]:
        raise Refusal("unknown engine or profile fields")
    return profile


def expand(value, paths):
    if not isinstance(value, str) or len(value) > 4096 or "\x00" in value:
        raise Refusal("invalid build argument")
    try:
        return value.format_map(paths)
    except (KeyError, ValueError, AttributeError, IndexError) as error:
        raise Refusal("unknown cache placeholder") from error


def command_for(project, profile, toolchain, paths):
    kind = profile["engine"]
    if kind == "script":
        executable = project_file(project, profile.get("executable"))
        if not executable.is_file() or not os.access(executable, os.X_OK):
            raise Refusal("build helper must be an executable project file")
        arguments = profile.get("arguments", [])
        if not isinstance(arguments, list) or len(arguments) > 64:
            raise Refusal("invalid helper arguments")
        return [str(executable), *(expand(arg, paths) for arg in arguments)]
    action = profile.get("action", "build")
    if action not in ("build", "test"):
        raise Refusal("native action must be build or test")
    if kind == "swiftpm":
        package = project_file(project, profile["package"]) if "package" in profile else project
        project_file(project, str((package / "Package.swift").relative_to(project)))
        config = profile.get("configuration", "debug")
        if config not in ("debug", "release"):
            raise Refusal("invalid SwiftPM configuration")
        argv = [toolchain["swift"], action, "--package-path", str(package), "--scratch-path", paths["scratch"],
                "--cache-path", paths["source_packages"], "--sdk", toolchain["sdk"], "--configuration", config]
        for key, option in (("product", "--product"), ("triple", "--triple")):
            if key in profile:
                if key == "product" and action == "test":
                    raise Refusal("SwiftPM test profiles cannot select a product")
                argv.extend([option, expand(profile[key], {})])
        if "jobs" in profile:
            jobs = profile["jobs"]
            if type(jobs) is not int or not 1 <= jobs <= 64:
                raise Refusal("jobs must be an integer between 1 and 64")
            argv.extend(["--jobs", str(jobs)])
        return argv
    containers = [key for key in ("project", "workspace") if key in profile]
    if len(containers) != 1:
        raise Refusal("Xcode needs exactly one project or workspace")
    key = containers[0]
    container = project_file(project, profile[key])
    argv = [toolchain["xcodebuild"], "-" + key, str(container)]
    for key in ("scheme", "configuration", "destination"):
        value = profile.get(key)
        if not isinstance(value, str) or not value or value.startswith("-"):
            raise Refusal("Xcode profiles must declare scheme, configuration and destination")
        argv.extend(["-" + key, expand(value, {})])
    argv.extend(["-sdk", toolchain["sdk"], "-derivedDataPath", paths["derived_data"], "-clonedSourcePackagesDirPath", paths["source_packages"]])
    settings = profile.get("settings", {})
    if not isinstance(settings, dict) or len(settings) > 32:
        raise Refusal("invalid Xcode settings")
    for key, value in sorted(settings.items()):
        if key not in XCODE_SETTINGS:
            raise Refusal("Xcode setting is not in the native adapter allowlist; use an explicit project helper")
        argv.append(key + "=" + expand(value, {}))
    argv.extend(["CLANG_MODULE_CACHE_PATH=" + paths["module_cache"], action])
    return argv


def prepare(project, name, generation="default", toolchain_probe=apple_toolchain):
    project = Path(project).resolve(strict=True)
    environment = base_environment()
    root = Path(probe(["/usr/bin/git", "-C", str(project), "rev-parse", "--show-toplevel"], environment)).resolve(strict=True)
    if project != root:
        raise Refusal("project must be the physical Git checkout root")
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,63}", generation):
        raise Refusal("invalid cache generation label")
    profile = configuration(project, name)
    sdk = profile.get("sdk", "macosx")
    if sdk not in ("macosx", "iphoneos", "iphonesimulator", "appletvos", "appletvsimulator", "watchos", "watchsimulator", "xros", "xrsimulator"):
        raise Refusal("unsupported Apple SDK name")
    tools = toolchain_probe(environment, sdk)
    info = project.stat()
    owner = {"schema_version": 1, "project": digest([str(project), info.st_dev, info.st_ino])}
    recipe = None
    if profile["engine"] == "script":
        recipe_path = project_file(project, profile.get("executable"))
        recipe = hashlib.sha256(recipe_path.read_bytes()).hexdigest()
    key = digest([owner, profile, tools, generation, recipe])
    cache = project / ".glaeda/apple-build/cache" / key
    paths = {name: str(cache / name) for name in CACHE_NAMES}
    argv = command_for(project, profile, tools, paths)
    configured = profile.get("environment", {})
    if not isinstance(configured, dict) or len(configured) > 32:
        raise Refusal("invalid build environment")
    for variable, value in configured.items():
        if not re.fullmatch(r"[A-Z][A-Z0-9_]*", variable) or variable in ENV_NAMES or variable in {"DEVELOPER_DIR", "SDKROOT", "TOOLCHAINS", "DYLD_INSERT_LIBRARIES", "DYLD_LIBRARY_PATH"}:
            raise Refusal("build environment overrides a reserved variable")
        environment[variable] = expand(value, paths)
    environment["DEVELOPER_DIR"] = tools["developer"]
    environment["CLANG_MODULE_CACHE_PATH"] = paths["module_cache"]
    return {"project": project, "profile": name, "owner": owner, "key": key, "paths": paths,
            "argv": argv, "environment": environment, "engine": profile["engine"], "generation": generation}


def directory(parent, name, create=False):
    made = False
    if create:
        try:
            os.mkdir(name, 0o700, dir_fd=parent)
            made = True
        except FileExistsError:
            pass
    fd = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent)
    info = os.fstat(fd)
    if info.st_uid != os.getuid() or info.st_mode & 0o022:
        os.close(fd)
        raise Refusal("state directory ownership or permissions are unsafe")
    return fd, made


def read_json(fd, name):
    file = os.open(name, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=fd)
    with os.fdopen(file, "rb") as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_nlink != 1 or info.st_mode & 0o077:
            raise Refusal("state file ownership or permissions are unsafe")
        return bounded_json(stream.read(LIMIT + 1))


def write_json(fd, name, document):
    temporary = "write-" + uuid.uuid4().hex
    file = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=fd)
    try:
        with os.fdopen(file, "w") as stream:
            json.dump(document, stream, sort_keys=True)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.rename(temporary, name, src_dir_fd=fd, dst_dir_fd=fd)
        os.fsync(fd)
    finally:
        with contextlib.suppress(FileNotFoundError):
            os.unlink(temporary, dir_fd=fd)


@contextlib.contextmanager
def store(plan, create=False):
    descriptors = [os.open(plan["project"], os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)]
    try:
        info = os.fstat(descriptors[0])
        if digest([str(plan["project"]), info.st_dev, info.st_ino]) != plan["owner"]["project"]:
            raise Refusal("checkout identity changed before state access")
        parent, _ = directory(descriptors[-1], ".glaeda", create)
        descriptors.append(parent)
        state, made = directory(parent, "apple-build", create)
        descriptors.append(state)
        if made:
            write_json(state, "owner.json", plan["owner"])
        if read_json(state, "owner.json") != plan["owner"]:
            raise Refusal("state belongs to another checkout; no adoption performed")
        yield state
    finally:
        for descriptor in reversed(descriptors):
            os.close(descriptor)


def inspect(plan):
    status = "cold"
    active = None
    try:
        with store(plan) as state:
            try:
                read_json(state, "quarantine-" + plan["key"] + ".json")
            except FileNotFoundError:
                pass
            else:
                raise Refusal("cache was interrupted; choose a new --generation for a cold rebuild")
            try:
                active = read_json(state, "inflight.json")["run_id"]
                status = "interrupted_or_running"
            except FileNotFoundError:
                try:
                    caches, _ = directory(state, "cache")
                    try:
                        cache, _ = directory(caches, plan["key"])
                        try:
                            try:
                                owner = read_json(cache, "owner.json")
                            except FileNotFoundError as error:
                                raise Refusal("existing cache generation is unmarked") from error
                            if owner != {**plan["owner"], "key": plan["key"]}:
                                raise Refusal("cache generation identity mismatch")
                            for name in CACHE_NAMES:
                                try:
                                    folder, _ = directory(cache, name)
                                except FileNotFoundError:
                                    continue
                                os.close(folder)
                            status = "prepared"
                        finally:
                            os.close(cache)
                    finally:
                        os.close(caches)
                except FileNotFoundError:
                    pass
    except FileNotFoundError:
        # Missing state is cold, but an existing unmarked state directory is ambiguous.
        if (plan["project"] / ".glaeda/apple-build").exists():
            raise Refusal("existing Apple state is incomplete; refusing implicit adoption")
    return {"schema_version": 1, "authority": "developer_observation_only", "profile": plan["profile"],
            "engine": plan["engine"], "cache_key": plan["key"], "generation": plan["generation"],
            "state": status, "active_run": active, "isolation": "trusted_native_host", "result_reuse": False}


@contextlib.contextmanager
def lock(state):
    fd = os.open("build.lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600, dir_fd=state)
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_nlink != 1 or info.st_mode & 0o077:
            raise Refusal("build lock is unsafe")
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise Refusal("another Glaeda Apple build owns this checkout") from error
        yield
    finally:
        os.close(fd)


def group_absent(pgid):
    if type(pgid) is not int or pgid <= 1:
        return False
    try:
        os.killpg(pgid, 0)
        return False
    except ProcessLookupError:
        return True


def source_snapshot(plan):
    git = ["/usr/bin/git", "-C", str(plan["project"])]
    environment = base_environment()
    commit = probe([*git, "rev-parse", "HEAD"], environment)
    if not re.fullmatch(r"[a-f0-9]{40,64}", commit):
        raise Refusal("project HEAD identity is invalid")
    return {"commit": commit, "clean": not bool(probe([*git, "status", "--porcelain"], environment))}


def execute(plan, prepare_again=prepare):
    inspect(plan)
    with store(plan, create=True) as state, lock(state):
        try:
            read_json(state, "inflight.json")
        except FileNotFoundError:
            pass
        else:
            raise Refusal("unfinished build recorded; use plan, then recover its exact run id")
        # Repeat admission after acquiring the project lock, before creating reusable state.
        fresh = prepare_again(plan["project"], plan["profile"], plan["generation"])
        if fresh["key"] != plan["key"]:
            raise Refusal("build configuration or toolchain changed during admission")
        caches, _ = directory(state, "cache", True)
        try:
            cache, made = directory(caches, plan["key"], True)
            try:
                expected = {**plan["owner"], "key": plan["key"]}
                if made:
                    write_json(cache, "owner.json", expected)
                if read_json(cache, "owner.json") != expected:
                    raise Refusal("cache generation identity mismatch")
                for name in CACHE_NAMES:
                    folder, _ = directory(cache, name, True)
                    os.close(folder)
            finally:
                os.close(cache)
        finally:
            os.close(caches)
        run_id = uuid.uuid4().hex
        source_before = source_snapshot(plan)
        active = {"schema_version": 1, "run_id": run_id, "cache_key": plan["key"], "pgid": None}
        write_json(state, "inflight.json", active)
        started = time.monotonic()
        child = None
        handlers = {}
        interrupted = []
        try:
            log_fd = os.open("run-" + run_id + ".log", os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=state)
            with os.fdopen(log_fd, "wb") as log:
                child = subprocess.Popen(plan["argv"], cwd=plan["project"], env=plan["environment"],
                                         stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            active["pgid"] = child.pid
            write_json(state, "inflight.json", active)
            def forward(number, frame):
                interrupted.append(number)
                with contextlib.suppress(ProcessLookupError):
                    os.killpg(child.pid, number)
            for signum in (signal.SIGINT, signal.SIGTERM):
                handlers[signum] = signal.signal(signum, forward)
            code = child.wait()
            deadline = time.monotonic() + 2
            while not group_absent(child.pid) and time.monotonic() < deadline:
                time.sleep(0.05)
            if not group_absent(child.pid):
                raise Refusal("build descendants remain; state kept unfinished")
            if interrupted:
                code = -interrupted[0]
            receipt = {**inspect(plan), "state": "completed", "active_run": None, "run_id": run_id,
                       "exit_code": code if code >= 0 else 128 - code, "signal": -code if code < 0 else None,
                       "elapsed_seconds": round(time.monotonic() - started, 6), "validation": "native_command_ran",
                       "source_before": source_before, "source_after": source_snapshot(plan)}
            write_json(state, "last-run.json", receipt)
            if code < 0:
                write_json(state, "quarantine-" + plan["key"] + ".json", receipt)
            os.unlink("inflight.json", dir_fd=state)
            os.fsync(state)
            return receipt
        except OSError:
            # A failed spawn has no child. Once spawned, unknown state stays blocked.
            if child is None:
                os.unlink("inflight.json", dir_fd=state)
                os.fsync(state)
            raise
        finally:
            for signum, handler in handlers.items():
                signal.signal(signum, handler)


def recover(plan, run_id):
    with store(plan) as state, lock(state):
        active = read_json(state, "inflight.json")
        if active.get("run_id") != run_id or not group_absent(active.get("pgid")):
            raise Refusal("recovery requires the exact run id and an absent build process group")
        key = active.get("cache_key")
        if not isinstance(key, str) or not re.fullmatch(r"[a-f0-9]{64}", key):
            raise Refusal("interrupted cache identity is invalid")
        receipt = {"schema_version": 1, "state": "interruption_recovered", "run_id": run_id,
                   "authority": "developer_observation_only", "result_reuse": False}
        write_json(state, "last-run.json", receipt)
        write_json(state, "quarantine-" + key + ".json", receipt)
        os.unlink("inflight.json", dir_fd=state)
        os.fsync(state)
        return receipt


def main():
    parser = argparse.ArgumentParser(description="Prepare and run trusted native Apple builds with persistent project caches")
    parser.add_argument("action", choices=("plan", "run", "warm", "recover"))
    parser.add_argument("--project", type=Path, default=Path.cwd())
    parser.add_argument("--profile", default="app")
    parser.add_argument("--generation", default="default", help="stable label; use a new label for a cold rebuild without deleting old caches")
    parser.add_argument("--run-id", help="exact interrupted run id, only for recover")
    args = parser.parse_args()
    try:
        if (args.action == "recover") != bool(args.run_id):
            raise Refusal("--run-id is required only for recover")
        plan = prepare(args.project, args.profile, args.generation)
        result = inspect(plan) if args.action == "plan" else recover(plan, args.run_id) if args.action == "recover" else execute(plan)
        print(json.dumps(result, sort_keys=True))
        return result.get("exit_code", 0)
    except (Refusal, OSError, ValueError, subprocess.SubprocessError) as error:
        # Exception paths may contain private project/toolchain paths; public failures do not.
        message = str(error) if isinstance(error, Refusal) else "native Apple build observation or execution failed"
        print(json.dumps({"schema_version": 1, "state": "refused", "reason": message}), file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
