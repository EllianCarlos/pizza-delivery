#!/usr/bin/env bash
# Deploy pizza-delivery from origin/main to the running user service.
#
#   scripts/deploy.sh [--deploy-dir DIR] [--unit UNIT] [--port PORT] [--no-restart]
#
# Steps: sync the deploy checkout to origin/main, build the web app, build the
# release binary, install it atomically at the path the unit's ExecStart runs,
# restart the unit, then verify the new process serves traffic.
set -euo pipefail

deploy_dir="${PIZZA_DELIVERY_DEPLOY_DIR:-$HOME/Projects/pizza-delivery-deploy}"
unit="${PIZZA_DELIVERY_UNIT:-pizza-delivery-daemon.service}"
port="${PIZZA_DELIVERY_HTTP_PORT:-7878}"
restart=1

usage() {
  cat <<EOF
usage: scripts/deploy.sh [options]

  --deploy-dir DIR  deploy checkout (default: \$PIZZA_DELIVERY_DEPLOY_DIR or ~/Projects/pizza-delivery-deploy)
  --unit UNIT       systemd user unit (default: pizza-delivery-daemon.service)
  --port PORT       HTTP port the daemon listens on (default: 7878)
  --no-restart      build and install the binary, but do not restart or verify
  -h, --help        show this help
EOF
}

step() { printf '\n==> %s\n' "$*"; }
die() {
  printf 'deploy: FAILED: %s\n' "$*" >&2
  exit 1
}

while [ $# -gt 0 ]; do
  case "$1" in
    --deploy-dir) [ $# -ge 2 ] || die "--deploy-dir needs a value"; deploy_dir="$2"; shift 2 ;;
    --unit) [ $# -ge 2 ] || die "--unit needs a value"; unit="$2"; shift 2 ;;
    --port) [ $# -ge 2 ] || die "--port needs a value"; port="$2"; shift 2 ;;
    --no-restart) restart=0; shift ;;
    -h | --help) usage; exit 0 ;;
    *) usage >&2; die "unknown argument: $1" ;;
  esac
done

for tool in git bun cargo curl; do
  command -v "$tool" >/dev/null 2>&1 || die "required tool not found on PATH: $tool"
done
if [ "$restart" -eq 1 ]; then
  for tool in systemctl ss; do
    command -v "$tool" >/dev/null 2>&1 || die "required tool not found on PATH: $tool"
  done
fi
# A worktree has a .git file, not a directory, so ask git instead.
git -C "$deploy_dir" rev-parse --is-inside-work-tree >/dev/null 2>&1 || die "$deploy_dir is not a git checkout"

step "sync $deploy_dir to origin/main"
git -C "$deploy_dir" fetch origin main || die "git fetch origin main failed"
git -C "$deploy_dir" checkout --detach origin/main || die "git checkout origin/main failed (local changes in the deploy checkout?)"
git -C "$deploy_dir" reset --hard origin/main || die "git reset --hard origin/main failed"
sha="$(git -C "$deploy_dir" rev-parse --short HEAD)"
printf 'deploying %s\n' "$sha"

step "build the web app"
(cd "$deploy_dir/web" && bun install --frozen-lockfile && bun run build) || die "web build failed"

step "build the release binary"
(cd "$deploy_dir" && cargo build --release -p pizza-delivery) || die "cargo build failed"
# The target dir may be redirected (CARGO_TARGET_DIR, config); ask cargo.
target_dir="$(cd "$deploy_dir" && cargo metadata --format-version 1 --no-deps |
  sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')" || die "cargo metadata failed"
[ -n "$target_dir" ] || die "could not read target_directory from cargo metadata"
built="$target_dir/release/pizza-delivery"
[ -x "$built" ] || die "built binary missing or not executable: $built"

# Where the unit actually runs the binary from.
dest="$deploy_dir/target/release/pizza-delivery"
if [ "$restart" -eq 1 ]; then
  exec_start="$(systemctl --user show -p ExecStart --value "$unit")" || die "cannot query unit $unit"
  unit_path="$(printf '%s\n' "$exec_start" | sed -n 's/.*[{ ]path=\([^ ;]*\) ;.*/\1/p' | head -n 1)"
  [ -n "$unit_path" ] || die "unit $unit has no ExecStart path (is it loaded?): $exec_start"
  dest="$unit_path"
fi

step "install $built -> $dest"
mkdir -p "$(dirname "$dest")" || die "cannot create $(dirname "$dest")"
cp "$built" "$dest.new" || die "copy to $dest.new failed"
mv -f "$dest.new" "$dest" || die "atomic rename to $dest failed"

if [ "$restart" -eq 0 ]; then
  step "done (--no-restart): $sha installed at $dest; service not restarted"
  exit 0
fi

step "restart $unit"
systemctl --user restart "$unit" || die "systemctl restart $unit failed"

step "verify"
pid=""
for _ in $(seq 1 50); do
  # Match only the process bound to 127.0.0.1:$port; never guess with pgrep.
  pid="$(ss -ltnpH "sport = :$port" 2>/dev/null |
    awk -v addr="127.0.0.1:$port" '$4 == addr' |
    sed -n 's/.*pid=\([0-9]*\).*/\1/p' | head -n 1)"
  [ -n "$pid" ] && break
  sleep 0.2
done
[ -n "$pid" ] || die "nothing listens on 127.0.0.1:$port after restart (journalctl --user -u $unit)"

main_pid="$(systemctl --user show -p MainPID --value "$unit")"
[ "$pid" = "$main_pid" ] || die "port $port is held by pid $pid, but $unit's MainPID is $main_pid"

state="$(systemctl --user is-active "$unit" || true)"
[ "$state" = "active" ] || die "$unit is $state, expected active"

exe="$(readlink "/proc/$pid/exe" 2>/dev/null || true)"
printf 'pid %s (%s) listens on 127.0.0.1:%s\n' "$pid" "${exe:-unknown}" "$port"

base="http://127.0.0.1:$port"
bytes="$(curl -fsS --max-time 10 "$base/app.js" | wc -c)" || die "GET $base/app.js failed"
[ "$bytes" -gt 0 ] || die "served app.js is empty"
printf 'app.js served, %s bytes\n' "$bytes"

step "deployed $sha successfully"
