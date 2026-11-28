#!/bin/bash
# Screen sharing services for Chromium (28.11.2026): the D-Bus session bus
# (dbus-daemon of Debian), PipeWire (pipewire of Debian) and fossai-portal
# (org.freedesktop.portal.Desktop ScreenCast + the screen streams from the
# kernel capture). Started by /opt/chromium/bin/chromium before the browser;
# a running service is left alone. Lines "screencast: ..." go to the kernel
# log, the services' own output to $XDG_RUNTIME_DIR/fossai-screencast/*.log.
C=${FOSSAI_SCREENCAST_ROOT:-/opt/chromium}
R=${XDG_RUNTIME_DIR:-/run/user/0}
export XDG_RUNTIME_DIR=$R
export LD_LIBRARY_PATH=$C/lib:$C/lib/pipewire-0.3
export PIPEWIRE_RUNTIME_DIR=$R
export PIPEWIRE_CONFIG_DIR=$C/share/pipewire
export SPA_PLUGIN_DIR=$C/lib/spa-0.2
export PIPEWIRE_MODULE_DIR=$C/lib/pipewire-0.3
export DBUS_SESSION_BUS_ADDRESS=unix:path=$R/bus
L=$R/fossai-screencast
mkdir -p "$L" || exit 0

log() {
	if [ -w /dev/kmsg ]; then printf 'screencast: %s\n' "$*" > /dev/kmsg; else printf 'screencast: %s\n' "$*" >&2; fi
}

alive() {
	[ -f "$L/$1.pid" ] && kill -0 "$(< "$L/$1.pid")" 2> /dev/null
}

# The output of a service: its log file and (pipewire, dbus - their
# warnings and errors only, see log.level) the kernel log.
forward() {
	local name=$1 line
	while IFS= read -r line; do
		printf '%s\n' "$line" >> "$L/$name.log"
		[ -w /dev/kmsg ] && printf '%s: %.300s\n' "$name" "$line" > /dev/kmsg
	done
}

# start NAME READY-FILE COMMAND...: run the service unless it runs, wait
# (up to 30 s - the emulator is slow) for the file it makes when ready.
# fossai-portal writes the kernel log itself: its output goes to the file.
start() {
	local name=$1 ready=$2 i=0 pid
	shift 2
	alive "$name" && return 0
	rm -f "$ready" "$L/$name.pid"
	: > "$L/$name.log"
	if [ "$name" = portal ]; then
		nohup "$@" >> "$L/$name.log" 2>&1 < /dev/null &
		echo $! > "$L/$name.pid"
	else
		# A plain pipe: no process substitution - FossAi has no /dev/fd.
		{
			trap '' HUP
			"$@" < /dev/null 2>&1 &
			echo $! > "$L/$name.pid"
			wait
		} 2> /dev/null < /dev/null | forward "$name" > /dev/null 2>&1 &
		while [ ! -s "$L/$name.pid" ] && [ $i -lt 100 ]; do
			sleep 0.05
			i=$((i + 1))
		done
		i=0
	fi
	pid=$(< "$L/$name.pid")
	while [ $i -lt 300 ]; do
		if [ -e "$ready" ]; then
			log "$name started (pid $pid, $((i / 10)).$((i % 10)) s)"
			return 0
		fi
		if ! kill -0 "$pid" 2> /dev/null; then
			break
		fi
		sleep 0.1
		i=$((i + 1))
	done
	log "$name did not start: $(tail -c 400 "$L/$name.log" | tr '\n' ' ')"
	return 1
}

start dbus "$R/bus" "$C/libexec/dbus-daemon" "--config-file=$C/share/dbus/session.conf" "--address=unix:path=$R/bus" --nofork --nopidfile || exit 0
start pipewire "$R/pipewire-0" "$C/libexec/pipewire" || exit 0
start portal "$L/portal.ready" env "FOSSAI_PORTAL_READY=$L/portal.ready" "$C/libexec/fossai-portal" || exit 0
exit 0
