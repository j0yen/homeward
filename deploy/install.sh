#!/usr/bin/env bash
# deploy/install.sh — idempotent homeward fleet installer
#
# Re-runnable.  Running it twice leaves exactly one copy of each unit.
# Does NOT start the fleet; use `homeward up` for that.
#
# Usage:
#   bash deploy/install.sh          # install
#   bash -n deploy/install.sh       # syntax check only (no --dry-run needed)
set -uo pipefail

DEPLOY_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SYSTEMD_USER_DIR="${HOME}/.config/systemd/user"
HOMEWARD_CFG_DIR="${HOME}/.config/homeward"
HOMEWARD_DATA_DIR="${HOME}/.local/share/homeward"
LOCAL_BIN="${HOME}/.local/bin"

# Unit files to install
UNITS=(
    homeward-embed.service
    homeward-ingest.service
    homeward-report.service
    homeward.target
)

log() { printf '[homeward-install] %s\n' "$*"; }
die() { printf '[homeward-install] ERROR: %s\n' "$*" >&2; exit 1; }

# ---------------------------------------------------------------------------
# 1. Directory layout
# ---------------------------------------------------------------------------
log "Creating directories..."
mkdir -p "${SYSTEMD_USER_DIR}"
mkdir -p "${HOMEWARD_CFG_DIR}"
mkdir -p "${HOMEWARD_DATA_DIR}"
mkdir -p "${LOCAL_BIN}"

# ---------------------------------------------------------------------------
# 2. Link systemd unit files (idempotent: -f overwrites stale symlinks)
# ---------------------------------------------------------------------------
log "Installing systemd user units..."
for unit in "${UNITS[@]}"; do
    src="${DEPLOY_DIR}/${unit}"
    dst="${SYSTEMD_USER_DIR}/${unit}"
    if [[ ! -f "${src}" ]]; then
        die "Missing unit file: ${src}"
    fi
    # Use symlink so edits in deploy/ take effect after daemon-reload
    ln -sf "${src}" "${dst}"
    log "  linked: ${dst} -> ${src}"
done

# ---------------------------------------------------------------------------
# 3. Seed env file if missing (never overwrites existing config)
# ---------------------------------------------------------------------------
ENV_FILE="${HOMEWARD_CFG_DIR}/homeward.env"
SAMPLE_FILE="${DEPLOY_DIR}/homeward.env.sample"
if [[ ! -f "${ENV_FILE}" ]]; then
    log "Seeding ${ENV_FILE} from sample..."
    cp "${SAMPLE_FILE}" "${ENV_FILE}"
    log "  IMPORTANT: edit ${ENV_FILE} before starting the fleet"
else
    log "  env file already exists: ${ENV_FILE} (not overwritten)"
fi

# ---------------------------------------------------------------------------
# 4. Install homeward wrapper script
# ---------------------------------------------------------------------------
HOMEWARD_SCRIPT="${DEPLOY_DIR}/homeward"
HOMEWARD_BIN="${LOCAL_BIN}/homeward"
if [[ ! -f "${HOMEWARD_SCRIPT}" ]]; then
    die "Missing wrapper script: ${HOMEWARD_SCRIPT}"
fi
ln -sf "${HOMEWARD_SCRIPT}" "${HOMEWARD_BIN}"
chmod +x "${HOMEWARD_SCRIPT}"
log "  linked: ${HOMEWARD_BIN} -> ${HOMEWARD_SCRIPT}"

# ---------------------------------------------------------------------------
# 5. Reload systemd user daemon
# ---------------------------------------------------------------------------
log "Reloading systemd user daemon..."
if systemctl --user daemon-reload; then
    log "  daemon-reload OK"
else
    log "  WARNING: daemon-reload failed (systemd may not be running)"
fi

log "Install complete."
log "Next steps:"
log "  1. Edit ${ENV_FILE} with your API keys and paths"
log "  2. Run: homeward up"
log "  3. Run: homeward status"
