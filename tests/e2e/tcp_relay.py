#!/usr/bin/env python3
"""Loopback TCP relay: 127.0.0.1:8443 -> $PHANTOM_RELAY_TARGET:8443.
Lets the HarmonyOS emulator (QEMU slirp, reaches host loopback via 10.0.2.2)
talk to the phantom server on the second Mac over the real WiFi link.
The upstream host is deliberately *not* hardcoded: pass the LAN address of the
machine running the test server via PHANTOM_RELAY_TARGET (no real addresses in
the repo).
Phase E test scaffolding; delete when Phase E is done."""
import os, socket, threading

TARGET = os.environ.get('PHANTOM_RELAY_TARGET', '<内网主机IP>')
TARGET_PORT = int(os.environ.get('PHANTOM_RELAY_TARGET_PORT', '8443'))

def pipe(a, b):
    try:
        while True:
            d = a.recv(262144)
            if not d:
                break
            b.sendall(d)
    except OSError:
        pass
    finally:
        try:
            b.shutdown(socket.SHUT_WR)
        except OSError:
            pass

srv = socket.socket()
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(('127.0.0.1', 8443))
srv.listen(64)
print(f'relay listening on 127.0.0.1:8443 -> {TARGET}:{TARGET_PORT}', flush=True)
while True:
    c, _ = srv.accept()
    try:
        u = socket.create_connection((TARGET, TARGET_PORT))
    except OSError as e:
        print(f'upstream connect failed: {e}', flush=True)
        c.close()
        continue
    threading.Thread(target=pipe, args=(c, u), daemon=True).start()
    threading.Thread(target=pipe, args=(u, c), daemon=True).start()
