#!/bin/bash
# Read-only bounded macOS scanner probe. Run with fleet-sudo.sh on one PR canary.
set -euo pipefail
[ "$(/usr/bin/id -u)" = 0 ] || { echo 'root required'; exit 1; }
probe_dir=$(/usr/bin/mktemp -d /private/tmp/glaeda-scan-probe.XXXXXX)
trap '/bin/rm -rf "$probe_dir"' EXIT
/usr/bin/fs_usage -ww -f filesystem -t 30 fseventsd XprotectService XProtectRemediatorColdSnap >"$probe_dir/fs" 2>/dev/null || true
/usr/bin/python3 - "$probe_dir/fs" <<'PY'
import collections, sys
counts=collections.Counter()
roots=(('DerivedData','/Library/Developer/Xcode/DerivedData/'),
       ('runner-work','/_work/'),('fleet-cache','/Users/Shared/cmux-build-fleet/'),
       ('CI-tmp','/private/tmp/cmux-'),('user-tmp','/private/var/folders/'))
for line in open(sys.argv[1],errors='replace'):
    daemon=next((name for name in ('fseventsd','XprotectService','XProtectRemediatorColdSnap') if name in line),None)
    if daemon is None: continue
    for label,root in roots:
        if root in line:
            counts[(daemon,label)]+=1
            break
for (daemon,root),count in sorted(counts.items()):
    print('fs_usage_rows',daemon,root,count)
PY
for daemon in fseventsd XprotectService; do
  pid=$(/usr/bin/pgrep -x "$daemon" | /usr/bin/head -1) || continue
  /usr/bin/sample "$pid" 3 1 -file "$probe_dir/sample" >/dev/null 2>&1 || continue
  /usr/bin/awk -v daemon="$daemon" '/Physical footprint:/{print daemon,$0} /in read\)|in lstat\)|in regncomp_l\)/{if(++n<=8) print daemon,$0}' "$probe_dir/sample"
done
/usr/bin/mdutil -s / /System/Volumes/Data 2>&1
/bin/ps -Ac -o comm= -o pcpu= | /usr/bin/awk '$1=="fseventsd" || $1=="XprotectService" || $1=="mediaanalysisd" || $1=="photoanalysisd" {print "cpu_pct",$0}'
echo 'bounded read-only scan probe complete; no host settings changed'
