#!/usr/bin/env bash
set -euo pipefail

if [[ -z "${INTAR_BIN:-}" ]]; then
  case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*)
      INTAR_BIN="target/debug/intar.exe"
      ;;
    *)
      INTAR_BIN="target/debug/intar"
      ;;
  esac
fi
TIMEOUT_SECS="${INTAR_E2E_TIMEOUT_SECS:-1800}"
SESSION_NAME="${INTAR_E2E_SESSION:-intar-e2e}"
TMUX_SERVER="${INTAR_E2E_TMUX_SERVER:-intar-e2e}"
UI_LOG="${INTAR_E2E_UI_LOG:-/tmp/intar-ui.log}"
LOG_SNAPSHOT_DIR="${INTAR_E2E_LOG_SNAPSHOT:-$PWD/e2e-logs}"

snapshot_logs() {
  mkdir -p "$LOG_SNAPSHOT_DIR"
  local bases=(
    "$HOME/.local/state/intar/runs"
    "$HOME/.local/share/intar/runs"
    "$HOME/Library/Application Support/intar/runs"
  )
  if command -v cygpath >/dev/null 2>&1 && [ -n "${LOCALAPPDATA:-}" ]; then
    bases+=("$(cygpath "$LOCALAPPDATA")/intar/runs")
  fi
  for base in "${bases[@]}"; do
    if [ -d "$base" ]; then
      while IFS= read -r -d '' dir; do
        rel="${dir#$base/}"
        dest="$LOG_SNAPSHOT_DIR/$rel"
        mkdir -p "$dest"
        while IFS= read -r -d '' file; do
          rel_file="${file#$dir/}"
          mkdir -p "$dest/$(dirname "$rel_file")"
          cp "$file" "$dest/$rel_file" 2>/dev/null || true
        done < <(find "$dir" -type f \( -name '*.log' -o -name '*.ndjson' \) -print0 2>/dev/null || true)
      done < <(find "$base" -type d -name logs -print0 2>/dev/null || true)
    fi
  done
  if [ -f "$UI_LOG" ]; then
    cp "$UI_LOG" "$LOG_SNAPSHOT_DIR/" 2>/dev/null || true
  fi
}

if [[ ! -x "$INTAR_BIN" ]]; then
  echo "intar binary not found at $INTAR_BIN"
  exit 1
fi

echo "Detecting hardware acceleration..."
case "$(uname -s)" in
  Linux)
    if [[ -e /dev/kvm ]] && [[ -r /dev/kvm ]] && [[ -w /dev/kvm ]]; then
      echo "HW accel: KVM available"
    else
      echo "HW accel: KVM unavailable"
    fi
    ;;
  Darwin)
    if command -v sysctl >/dev/null 2>&1; then
      if [[ "$(sysctl -n kern.hv_support 2>/dev/null)" == "1" ]]; then
        echo "HW accel: HVF available"
      else
        echo "HW accel: HVF unavailable"
      fi
    else
      echo "HW accel: HVF status unknown"
    fi
    ;;
  MINGW*|MSYS*|CYGWIN*)
    echo "HW accel: WHPX status unknown"
    ;;
  *)
    echo "HW accel: unknown"
    ;;
esac

TMUX_CMD=(tmux -L "$TMUX_SERVER")

cleanup() {
  snapshot_logs || true
  if command -v tmux >/dev/null 2>&1; then
    if "${TMUX_CMD[@]}" has-session -t "$SESSION_NAME" 2>/dev/null; then
      "${TMUX_CMD[@]}" send-keys -t "$SESSION_NAME" q >/dev/null 2>&1 || true
      for _ in $(seq 1 20); do
        if ! "${TMUX_CMD[@]}" has-session -t "$SESSION_NAME" 2>/dev/null; then
          break
        fi
        sleep 1
      done
      "${TMUX_CMD[@]}" kill-session -t "$SESSION_NAME" >/dev/null 2>&1 || true
    fi
  fi
}
trap cleanup EXIT

echo "Listing scenarios..."
"$INTAR_BIN" list --dir scenarios

if ! command -v tmux >/dev/null 2>&1; then
  echo "tmux is required for the e2e harness."
  exit 1
fi

if "${TMUX_CMD[@]}" has-session -t "$SESSION_NAME" 2>/dev/null; then
  "${TMUX_CMD[@]}" kill-session -t "$SESSION_NAME"
fi

echo "Starting broken-nginx scenario (interactive UI in tmux)..."
"${TMUX_CMD[@]}" new-session -d -s "$SESSION_NAME" "$INTAR_BIN" start scenarios/broken-nginx.hcl
"${TMUX_CMD[@]}" pipe-pane -t "$SESSION_NAME" -o "cat >> \"$UI_LOG\""

echo "Waiting for SSH to become available..."
ready=""
poll_secs=5
max_attempts=$((TIMEOUT_SECS / poll_secs))
if [[ $max_attempts -lt 1 ]]; then
  max_attempts=1
fi
for _ in $(seq 1 "$max_attempts"); do
  if "$INTAR_BIN" ssh webserver --command "true" >/dev/null 2>&1; then
    ready="yes"
    break
  fi
  sleep "$poll_secs"
done

if [[ -z "$ready" ]]; then
  echo "SSH never became ready."
  "$INTAR_BIN" logs --vm webserver --log-type console || true
  "${TMUX_CMD[@]}" capture-pane -pt "$SESSION_NAME" -S -2000 > "$UI_LOG" 2>/dev/null || true
  snapshot_logs
  exit 1
fi

echo "Waiting for nginx package install..."
nginx_ready=""
for _ in $(seq 1 "$max_attempts"); do
  if "$INTAR_BIN" ssh webserver --command "test -d /etc/nginx" >/dev/null 2>&1; then
    nginx_ready="yes"
    break
  fi
  sleep "$poll_secs"
done

if [[ -z "$nginx_ready" ]]; then
  echo "nginx package did not finish installing in time."
  "$INTAR_BIN" logs --vm webserver --log-type console || true
  "${TMUX_CMD[@]}" capture-pane -pt "$SESSION_NAME" -S -2000 > "$UI_LOG" 2>/dev/null || true
  snapshot_logs
  exit 1
fi

echo "Waiting for break-nginx step..."
break_ready=""
for _ in $(seq 1 "$max_attempts"); do
  if "$INTAR_BIN" ssh webserver --command "sudo -n test -f /var/log/intar/step-webserver-break-nginx.log && sudo -n grep -q \"step webserver/break-nginx complete\" /var/log/intar/step-webserver-break-nginx.log" >/dev/null 2>&1; then
    break_ready="yes"
    break
  fi
  sleep "$poll_secs"
done

if [[ -z "$break_ready" ]]; then
  echo "break-nginx step did not finish in time."
  "$INTAR_BIN" logs --vm webserver --log-type console || true
  "${TMUX_CMD[@]}" capture-pane -pt "$SESSION_NAME" -S -2000 > "$UI_LOG" 2>/dev/null || true
  snapshot_logs
  exit 1
fi

echo "Applying nginx fix..."
fix_ok=""
for _ in $(seq 1 "$max_attempts"); do
  if "$INTAR_BIN" ssh webserver --command "sudo -n ln -sfn /etc/nginx/sites-available/default /etc/nginx/sites-enabled/default" >/dev/null 2>&1; then
    fix_ok="yes"
    break
  fi
  sleep "$poll_secs"
done

if [[ -z "$fix_ok" ]]; then
  echo "Failed to apply nginx fix."
  "$INTAR_BIN" logs --vm webserver --log-type console || true
  "${TMUX_CMD[@]}" capture-pane -pt "$SESSION_NAME" -S -2000 > "$UI_LOG" 2>/dev/null || true
  snapshot_logs
  exit 1
fi

echo "Restarting nginx..."
restart_ok=""
for _ in $(seq 1 "$max_attempts"); do
  if "$INTAR_BIN" ssh webserver --command "sudo -n systemctl restart nginx" >/dev/null 2>&1; then
    restart_ok="yes"
    break
  fi
  sleep "$poll_secs"
done

if [[ -z "$restart_ok" ]]; then
  echo "Failed to restart nginx."
  "$INTAR_BIN" logs --vm webserver --log-type console || true
  "${TMUX_CMD[@]}" capture-pane -pt "$SESSION_NAME" -S -2000 > "$UI_LOG" 2>/dev/null || true
  snapshot_logs
  exit 1
fi

echo "Verifying nginx..."
verify_ok=""
for _ in $(seq 1 "$max_attempts"); do
  if "$INTAR_BIN" ssh webserver --command "sudo -n systemctl is-active --quiet nginx" >/dev/null 2>&1 \
    && "$INTAR_BIN" ssh webserver --command "test -f /etc/nginx/sites-enabled/default" >/dev/null 2>&1 \
    && "$INTAR_BIN" ssh webserver --command "curl -fsS --max-time 2 http://localhost >/dev/null" >/dev/null 2>&1; then
    verify_ok="yes"
    break
  fi
  sleep "$poll_secs"
done

if [[ -z "$verify_ok" ]]; then
  echo "nginx verification failed."
  "$INTAR_BIN" logs --vm webserver --log-type console || true
  "${TMUX_CMD[@]}" capture-pane -pt "$SESSION_NAME" -S -2000 > "$UI_LOG" 2>&1 || true
  snapshot_logs
  exit 1
fi

echo "Checking logs command..."
if ! "$INTAR_BIN" logs --vm webserver --log-type console > /dev/null; then
  echo "Logs command failed."
  "${TMUX_CMD[@]}" capture-pane -pt "$SESSION_NAME" -S -2000 > "$UI_LOG" 2>&1 || true
  snapshot_logs
  exit 1
fi

echo "Snapshotting logs before shutdown..."
snapshot_logs

echo "Requesting UI shutdown..."
if "${TMUX_CMD[@]}" has-session -t "$SESSION_NAME" 2>/dev/null; then
  "${TMUX_CMD[@]}" send-keys -t "$SESSION_NAME" q
fi

ui_gone=""
for _ in $(seq 1 30); do
  if ! "${TMUX_CMD[@]}" has-session -t "$SESSION_NAME" 2>/dev/null; then
    ui_gone="yes"
    break
  fi
  sleep 1
done

if [[ -z "$ui_gone" ]]; then
  echo "UI did not exit in time."
  "${TMUX_CMD[@]}" capture-pane -pt "$SESSION_NAME" -S -2000 > "$UI_LOG" 2>/dev/null || true
  snapshot_logs
  exit 1
fi

if "${TMUX_CMD[@]}" has-session -t "$SESSION_NAME" 2>/dev/null; then
  "${TMUX_CMD[@]}" kill-session -t "$SESSION_NAME" >/dev/null 2>&1 || true
fi

echo "E2E scenario completed successfully."
