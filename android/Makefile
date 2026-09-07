# Day-to-day Android build/debug workflow, so nobody has to remember the
# adb/gradle incantations (see README.md for the one-time SDK/NDK setup).
#
#   make build        ./gradlew assembleDebug (runs cargo-ndk itself)
#   make check        Rust-only lint for the Android target (no SDK needed)
#   make flash        build + install + launch on the connected phone
#   make screenshot   grab the phone's screen into /tmp, print the path
#   make log          follow the app's crashes/ANRs/Rust logs
#   make pair CODE=123456   one-time wireless-debugging pairing
#   make connect      attach to the phone over Wi-Fi (after pairing once)
#
# ANDROID_HOME defaults to ~/Android (an SDK root with an ndk/ inside, which
# is also where cargo-ndk finds the NDK); adb is taken from PATH with the
# ANDROID_HOME install as fallback. With several devices attached, pick one
# with SERIAL=..., e.g.
#   make screenshot SERIAL=192.168.1.185:33237

ANDROID_HOME ?= $(HOME)/Android
ADB ?= $(shell command -v adb 2>/dev/null || echo $(ANDROID_HOME)/platform-tools/adb)
GRADLE ?= ./gradlew

# adb target: explicit SERIAL, else whatever single device is attached.
ADB_S = $(ADB) $(if $(SERIAL),-s $(SERIAL))

APP = me.danielschaefer.tapview
APK = app/build/outputs/apk/debug/app-debug.apk

.PHONY: help build check flash install run stop screenshot log crash connect pair devices

help:
	@sed -n 's/^#   /  /p' Makefile

# Full app build; Gradle's cargoNdk task cross-builds the Rust side first.
build:
	ANDROID_HOME=$(ANDROID_HOME) $(GRADLE) assembleDebug

# Fast Rust-only lint of the whole lib for Android - needs the rustup
# target but neither the NDK nor Gradle, so it runs anywhere.
check:
	cd .. && cargo clippy --lib --target aarch64-linux-android -- -D warnings

flash: build install run

install:
	$(ADB_S) install -r $(APK)

run:
	$(ADB_S) shell am start -n $(APP)/.TapviewActivity

stop:
	$(ADB_S) shell am force-stop $(APP)

# Timestamped so repeated shots during a session don't overwrite each other.
screenshot:
	@out=/tmp/tapview-$$(date +%H%M%S).png; \
	$(ADB_S) exec-out screencap -p > $$out && echo $$out

# Follow the signals that matter: crashes, ANRs on this app, Rust panics and
# log lines. -T 1 starts at "now" rather than replaying the whole buffer.
log:
	$(ADB_S) logcat -T 1 | grep --line-buffered -E \
	  "FATAL EXCEPTION|AndroidRuntime: Caused|Fatal signal|SIGSEGV|SIGABRT|ANR in $(APP)|I tapview |RustStdoutStderr|$(APP): panic"

# The most recent crash/ANR verdicts, for when the app already died.
crash:
	$(ADB_S) logcat -d -b crash | grep -B2 -A20 "FATAL EXCEPTION" | tail -40 || true
	$(ADB_S) logcat -d | grep -A3 "ANR in $(APP)" | tail -20 || true

# Wireless debugging. Pairing is one-time per machine: enable Developer
# options > Wireless debugging > "Pair device with pairing code" on the
# phone, then `make pair CODE=<the 6 digits>`. From then on `make connect`
# suffices whenever phone and machine share a network - the ports are
# discovered over mDNS, so they may change between sessions without mattering.
connect:
	@if $(ADB) devices | awk 'NR>1 && $$2=="device"' | grep -q .; then \
	  echo "already connected:"; $(ADB) devices | sed 1d; exit 0; \
	fi; \
	port=$$($(ADB) mdns services | awk '/_adb-tls-connect/ {print $$NF}' | head -1); \
	test -n "$$port" || { echo "no phone advertising wireless debugging found (same Wi-Fi? toggle Wireless debugging?)"; exit 1; }; \
	$(ADB) connect $$port

pair:
	@test -n "$(CODE)" || { echo "usage: make pair CODE=123456 (from the phone's pairing dialog)"; exit 1; }
	@port=$$($(ADB) mdns services | awk '/_adb-tls-pairing/ {print $$NF}' | head -1); \
	test -n "$$port" || { echo "no pairing service found - is the pairing dialog open on the phone?"; exit 1; }; \
	$(ADB) pair $$port $(CODE)

devices:
	$(ADB) devices -l
