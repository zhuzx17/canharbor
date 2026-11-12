#!/usr/bin/env python3
"""Run one virtual CAN exchange in an isolated local service."""
import argparse
import json
import pathlib
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=pathlib.Path,
                        default=pathlib.Path(__file__).resolve().parents[1] / 'target/debug/canharbor')
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.is_file():
        parser.error('Build the CLI first: cargo build -p canharbor-cli --locked')
    with tempfile.TemporaryDirectory(prefix='ch-demo-', dir='/tmp') as runtime:
        command = [str(binary), '--runtime-dir', runtime]
        listener = None
        with tempfile.TemporaryFile(mode='w+') as log:
            service = subprocess.Popen(command + ['serve', '--no-web'], stdout=log, stderr=log)
            def cli(*parts):
                return subprocess.run(command + list(parts), capture_output=True, text=True, timeout=5)
            try:
                deadline = time.monotonic() + 15
                while cli('devices').returncode:
                    if service.poll() is not None or time.monotonic() >= deadline:
                        raise RuntimeError('Local service did not start')
                    time.sleep(0.1)
                print('Simulation only: virtual:0 → virtual:1', flush=True)
                listener = subprocess.Popen(command + ['monitor', '--channel', 'virtual:1',
                    '--listen-only', '--count', '1'], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                while True:
                    result = cli('session', 'list')
                    inventory = json.loads(result.stdout) if result.returncode == 0 else {}
                    sessions = inventory.get('sessions', []) if isinstance(inventory, dict) else inventory
                    if any(s.get('channel_id') == 'virtual:1' and s.get('state') == 'running' for s in sessions):
                        break
                    if listener.poll() is not None or time.monotonic() >= deadline:
                        raise RuntimeError('Virtual listener did not start')
                    time.sleep(0.1)
                result = cli('send', '--channel', 'virtual:0', '--id', '123', '--data', '01020304', '--active')
                if result.returncode:
                    raise RuntimeError(result.stderr.strip())
                output, error = listener.communicate(timeout=10)
                if listener.returncode:
                    raise RuntimeError(error.strip())
                print(output.strip())
                print('Virtual exchange complete. No physical device was opened.')
            finally:
                if listener is not None and listener.poll() is None:
                    listener.terminate()
                    listener.communicate(timeout=5)
                service.terminate()
                service.wait(timeout=10)


if __name__ == '__main__':
    main()
