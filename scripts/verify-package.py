#!/usr/bin/env python3
"""Verify packaged archives and smoke-test the extracted native CLI (Python 3.11+)."""
import argparse
import stat
import sys
import zipfile
import hashlib
import io
import json
import os
import pathlib
import platform
import re
import signal
import struct
import subprocess
import tarfile
import tempfile
import time
import tomllib
import urllib.parse
import urllib.request
from http.cookiejar import CookieJar


TARGETS = json.loads(pathlib.Path(__file__).with_name('package-targets.json').read_text())

def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def members(archive):
    entries = archive.getmembers()
    result = {entry.name: entry for entry in entries}
    require(len(result) == len(entries), 'Duplicate archive member names')
    return result


class ZipArchive:
    """Expose ordinary ZIP files/directories through the same strict archive checks."""
    def __init__(self, path):
        self.archive = zipfile.ZipFile(path)
    def __enter__(self):
        return self
    def __exit__(self, *args):
        self.archive.close()
    def getmembers(self):
        result = []
        for info in self.archive.infolist():
            mode = info.external_attr >> 16
            require(stat.S_IFMT(mode) in (0, stat.S_IFREG, stat.S_IFDIR), 'Unsafe ZIP entry type')
            entry = tarfile.TarInfo(info.filename.rstrip('/'))
            entry.type = tarfile.DIRTYPE if info.is_dir() else tarfile.REGTYPE
            entry.mode, entry.size = stat.S_IMODE(mode), info.file_size
            result.append(entry)
        return result
    def extractfile(self, entry):
        return self.archive.open(entry.name)
    def extractall(self, destination, **kwargs):
        # Names and types were validated by verify_archives before any write.
        for entry in self.getmembers():
            path = destination.joinpath(*pathlib.PurePosixPath(entry.name).parts)
            if entry.isdir():
                path.mkdir(parents=True, exist_ok=True)
            else:
                path.parent.mkdir(parents=True, exist_ok=True)
                with self.extractfile(entry) as source, path.open('xb') as target:
                    import shutil
                    shutil.copyfileobj(source, target)


def binary_archive(path):
    return ZipArchive(path) if path.suffix == '.zip' else tarfile.open(path)


def windows_imports(data):
    """Read bounded PE32+ import descriptors (Microsoft PE/COFF layout)."""
    def field(fmt, offset):
        require(0 <= offset <= len(data) - struct.calcsize(fmt), 'Truncated PE import metadata')
        return struct.unpack_from(fmt, data, offset)
    pe, = field('<I', 60)
    require(data[:2] == b'MZ' and data[pe:pe+4] == b'PE\0\0', 'Invalid PE signature')
    sections, = field('<H', pe+6)
    optional_size, = field('<H', pe+20)
    optional = pe+24
    require(field('<H', optional)[0] == 0x20b and optional_size >= 240 and sections <= 96,
            'Expected bounded PE32+ import metadata')
    directories, = field('<I', optional+108)
    require(14 <= directories <= (optional_size-112)//8, 'PE import directories missing')
    require(field('<II', optional+112+13*8) == (0, 0), 'Delayed imports require explicit dependency review')
    headers, = field('<I', optional+60)
    table = optional+optional_size
    spans = [field('<IIII', table+i*40+8) for i in range(sections)]
    def location(rva, size):
        if rva < headers and rva+size <= min(headers, len(data)):
            return rva
        for _, address, raw_size, raw_offset in spans:
            if address <= rva and rva-address+size <= raw_size:
                offset = raw_offset+rva-address
                require(offset+size <= len(data), 'Truncated PE section')
                return offset
        raise RuntimeError('Invalid PE import RVA')
    rva, size = field('<II', optional+120)
    if rva == size == 0:
        return []
    require(rva > 0 and 20 <= size <= 1024*1024, 'Invalid PE import directory')
    names = []
    for index in range(min(size//20, 4096)):
        entry = field('<IIIII', location(rva+index*20, 20))
        if entry == (0, 0, 0, 0, 0):
            return names
        require(entry[3] > 0, 'Invalid PE DLL name RVA')
        name = bytearray()
        for i in range(260):
            byte = data[location(entry[3]+i, 1)]
            if byte == 0:
                break
            name.append(byte)
        else:
            raise RuntimeError('Unterminated PE DLL name')
        require(name and all(32 < b < 127 for b in name), 'Invalid PE DLL name')
        name = name.decode('ascii').lower()
        require(re.fullmatch(r'[a-z0-9_-]+\.dll', name), 'Invalid PE DLL basename')
        names.append(name)
    raise RuntimeError('Unterminated PE import directory')


def verify_windows_dependencies(data):
    # OS DLLs/API sets are provided by Windows 11. VC redistributable DLLs and
    # arbitrary toolchain/third-party DLLs must not be hidden prerequisites.
    system = {'advapi32.dll', 'bcrypt.dll', 'bcryptprimitives.dll', 'cfgmgr32.dll',
              'crypt32.dll', 'dnsapi.dll', 'gdi32.dll', 'iphlpapi.dll', 'kernel32.dll',
              'kernelbase.dll', 'msvcrt.dll', 'normaliz.dll', 'ntdll.dll', 'ole32.dll',
              'oleaut32.dll', 'rpcrt4.dll', 'secur32.dll', 'setupapi.dll', 'shell32.dll',
              'shlwapi.dll', 'user32.dll', 'userenv.dll', 'version.dll', 'winmm.dll',
              'winusb.dll', 'ws2_32.dll', 'wtsapi32.dll'}
    names = windows_imports(data)
    unexpected = sorted({name for name in names if name not in system
                         and not name.startswith(('api-ms-win-', 'ext-ms-win-'))})
    require(not unexpected, f'Non-system Windows runtime DLLs: {unexpected}')
    return sorted(set(names))


def verify_archives(repository, packages, target, extract):
    revision = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=repository, text=True).strip()
    name = f'canharbor-{revision[:12]}-{target}'
    target_spec = TARGETS[target]
    archives = [name + target_spec['extension'], f'canharbor-{revision[:12]}-source.tar.gz']
    expected_files = set(archives + [archive + '.sha256' for archive in archives])
    require({path.name for path in packages.iterdir()} == expected_files,
            'Package directory must contain exactly this revision/platform, source and checksums')
    for filename in archives:
        fields = (packages / (filename + '.sha256')).read_text().split()
        require(len(fields) == 2 and fields[1] == filename
                and re.fullmatch(r'[0-9a-f]{64}', fields[0]), f'Invalid checksum file: {filename}')
        with (packages / filename).open('rb') as stream:
            digest = hashlib.file_digest(stream, 'sha256').hexdigest()
        require(digest == fields[0], f'SHA-256 mismatch: {filename}')

    # Compare all source bytes and modes against the checked-out commit, not
    # just the revision printed in START.txt or a checksum supplied by the packager.
    committed = subprocess.check_output(
        ['git', 'archive', '--format=tar', '--prefix=canharbor-source/', revision], cwd=repository)
    source_files = {}
    with tarfile.open(fileobj=io.BytesIO(committed)) as expected, tarfile.open(packages / archives[1]) as source:
        expected_members, source_members = members(expected), members(source)
        require(expected_members.keys() == source_members.keys(), 'Source archive inventory differs from git')
        for path, entry in expected_members.items():
            actual = source_members[path]
            require((entry.type, entry.mode, entry.linkname) == (actual.type, actual.mode, actual.linkname),
                    f'Source metadata differs from git: {path}')
            if entry.isfile():
                content = source.extractfile(actual).read()
                require(content == expected.extractfile(entry).read(), f'Source content differs from git: {path}')
                source_files[path.removeprefix('canharbor-source/')] = content

    launcher_name = target_spec['launcher']
    binary_name = target_spec['binary']
    with binary_archive(packages / archives[0]) as archive:
        entries = members(archive)
        require(len(entries) < 10000 and sum(e.size for e in entries.values()) < 2**31, 'Binary archive exceeds limits')
        if target == 'windows-x86_64':
            require(len({p.casefold() for p in entries}) == len(entries), 'Ambiguous Windows archive names')
        for path, entry in entries.items():
            parts = pathlib.PurePosixPath(path).parts
            require(parts and parts[0] == name and '..' not in parts
                    and not pathlib.PurePosixPath(path).is_absolute()
                    and '\\' not in path and ':' not in path
                    and all(p and p not in ('.', '..') and not p.endswith((' ', '.')) for p in parts)
                    and (entry.isfile() or entry.isdir()), f'Unsafe binary archive member: {path}')
        for filename in [binary_name, launcher_name, 'START.txt', 'LICENSE', 'THIRD_PARTY_NOTICES', 'LICENSES']:
            require(f'{name}/{filename}' in entries, f'Missing packaged file: {filename}')
        for filename in [binary_name, launcher_name, 'START.txt', 'LICENSE', 'THIRD_PARTY_NOTICES']:
            require(entries[f'{name}/{filename}'].isfile(), f'Expected packaged regular file: {filename}')
        require(entries[f'{name}/LICENSES'].isdir(), 'Expected packaged LICENSES directory')
        for filename in ([] if target == 'windows-x86_64' else [binary_name, launcher_name]):
            entry = entries[f'{name}/{filename}']
            require(entry.isfile() and entry.mode & 0o111, f'Packaged executable is not executable: {filename}')
        for path, content in source_files.items():
            if path in ['LICENSE', 'THIRD_PARTY_NOTICES'] or path.startswith('LICENSES/'):
                entry = entries.get(f'{name}/{path}')
                require(entry is not None and entry.isfile(), f'Missing packaged license: {path}')
                require(archive.extractfile(entry).read() == content, f'Packaged license differs: {path}')
        start = archive.extractfile(entries[f'{name}/START.txt']).read().decode()
        require(f'Source revision: {revision}\n' in start, 'START.txt source revision mismatch')
        require('Start:' in start and '启动：' in start and '--lang zh-CN --help' in start,
                'Bilingual launch instructions missing')
        header = archive.extractfile(entries[f'{name}/{binary_name}']).read(4096)
        if target == 'linux-x86_64':
            require(len(header) >= 20 and header[:6] == b'\x7fELF\x02\x01'
                    and struct.unpack_from('<H', header, 18)[0] == 62, 'Expected an ELF64 x86-64 binary')
        elif target == 'macos-arm64':
            require(len(header) >= 8 and struct.unpack_from('<II', header) == (0xfeedfacf, 0x0100000c),
                    'Expected a Mach-O ARM64 binary')
        else:
            require(len(header) >= 64 and header[:2] == b'MZ', 'Expected a PE32+ x86-64 binary')
            offset = struct.unpack_from('<I', header, 60)[0]
            require(64 <= offset <= len(header) - 26 and header[offset:offset+4] == b'PE\0\0'
                    and struct.unpack_from('<H', header, offset+4)[0] == 0x8664
                    and struct.unpack_from('<H', header, offset+24)[0] == 0x20b,
                    'Expected a PE32+ x86-64 binary')
            require(entries[f'{name}/{binary_name}'].size <= 128*1024*1024, 'Windows binary exceeds audit limit')
            verify_windows_dependencies(archive.extractfile(entries[f'{name}/{binary_name}']).read())
        archive.extractall(extract, filter='data')
    version = tomllib.loads(source_files['Cargo.toml'].decode())['workspace']['package']['version']
    folder = extract / name
    if target != 'windows-x86_64' and os.name != 'nt':
        subprocess.run(['sh', '-n', str(folder / launcher_name)], check=True, timeout=10)
    return folder, launcher_name, version, revision


def cleanup(process):
    if process is not None and process.poll() is None:
        process.kill()
        process.communicate(timeout=10)


def smoke(folder, launcher_name, version, runtime):
    windows = platform.system() == 'Windows'
    binary = str(folder / ('canharbor.exe' if windows else 'canharbor'))
    env = {**os.environ, 'TMPDIR': str(runtime), 'LC_ALL': 'C',
           'CANHARBOR_CONFIG_DIR': str(runtime / 'preferences'),
           'LOCALAPPDATA': str(runtime), 'PYTHONIOENCODING': 'utf-8'}
    service = listener = None
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}),
                                        urllib.request.HTTPCookieProcessor(CookieJar()))

    def cli(*args):
        result = subprocess.run([binary, *args], env=env, capture_output=True, text=True, encoding='utf-8', timeout=20)
        require(result.returncode == 0, f'CLI {args} exited {result.returncode}: {result.stderr}')
        return result.stdout

    def get(path):
        with opener.open(origin + path, timeout=10) as response:
            require(response.status == 200, f'HTTP {path}: {response.status}')
            return response.read()

    with tempfile.TemporaryFile(mode='w+', encoding='utf-8') as log:
        try:
            service = subprocess.Popen([binary, '--lang', 'en', 'serve', '--port', '0'], env=env,
                                       stdout=log, stderr=log,
                                       creationflags=subprocess.CREATE_NEW_CONSOLE if windows else 0)
            deadline = time.monotonic() + 30
            origin = None
            while time.monotonic() < deadline:
                log.seek(0)
                output = log.read()
                require(service.poll() is None, f'Packaged service exited before readiness: {output}')
                ready = re.search(r'local service ready at (http://127\.0\.0\.1:\d+)', output)
                if ready:
                    origin = ready.group(1)
                    break
                time.sleep(0.05)
            require(origin is not None, 'Packaged service readiness deadline')
            require('Usage:' in cli('--lang', 'en', '--help'), 'Packaged English help missing')
            require('用法：' in cli('--lang', 'zh-CN', '--help'), 'Packaged Chinese help missing')
            require(cli('--lang', 'en', 'devices', '--json') == cli('--lang', 'zh-CN', 'devices', '--json'),
                    'Language changed packaged device JSON')
            devices = json.loads(cli('devices', '--json'))['devices']
            require(all(device['simulated'] for device in devices), 'Smoke test must use virtual devices only')
            channels = sorted(channel['id'] for device in devices for channel in device['channels'])
            require(channels == ['virtual:0', 'virtual:1'], 'Expected two packaged virtual channels')
            reported = json.loads(get('/api/v1/version'))
            require(reported['version'] == version and reported['web_ui'] is True,
                    'Packaged version or embedded web UI mismatch')
            html = get('/').decode()
            require('CANHarbor' in html, 'Embedded workbench HTML missing')
            assets = re.findall(r'(?:src|href)="(/assets/[A-Za-z0-9_./-]+)"', html)
            require(any(path.endswith('.js') for path in assets) and any(path.endswith('.css') for path in assets),
                    'Embedded JavaScript or CSS missing')
            for path in assets:
                require(len(get(path)) > 100, f'Empty embedded asset: {path}')

            # Exercise the extracted launcher, stubbing only the OS browser
            # opener. Its real CLI must reuse this isolated default service.
            link = runtime / 'opened-url'
            if windows:
                # --print exercises service reuse and pairing without changing a user's browser.
                result = subprocess.run([os.environ.get('COMSPEC', 'cmd.exe'), '/d', '/c', launcher_name, '--print'],
                                        cwd=folder, env=env, capture_output=True, text=True, encoding='utf-8', timeout=20)
                require(result.returncode == 0, f'Extracted launcher failed: {result.stderr}')
                link.write_text(result.stdout.strip(), encoding='utf-8')
            else:
                stub = runtime / 'bin'
                stub.mkdir()
                browser = stub / ('open' if platform.system() == 'Darwin' else 'xdg-open')
                browser.write_text('#!/bin/sh\nset -eu\nprintf "%s\\n" "$1" > "$CANHARBOR_SMOKE_OPEN_LOG"\n')
                browser.chmod(0o755)
                link = runtime / 'opened-url'
                result = subprocess.run([str(folder / launcher_name)], cwd='/tmp', timeout=20,
                                        env={**env, 'PATH': str(stub) + os.pathsep + env['PATH'],
                                             'CANHARBOR_SMOKE_OPEN_LOG': str(link)}, capture_output=True, text=True)
                require(result.returncode == 0 and link.is_file(), f'Extracted launcher failed: {result.stderr}')
            url = urllib.parse.urlsplit(link.read_text().strip())
            require(f'{url.scheme}://{url.netloc}' == origin and url.path == '/', 'Launcher did not reuse service')
            codes = urllib.parse.parse_qs(url.fragment).get('pair', [])
            require(len(codes) == 1, 'Launcher did not create a browser pairing link')
            request = urllib.request.Request(origin + '/api/v1/auth/pair',
                      data=json.dumps({'code': codes[0]}).encode(),
                      headers={'Content-Type': 'application/json', 'Origin': origin}, method='POST')
            with opener.open(request, timeout=10) as response:
                paired = json.load(response)
            require(json.loads(get('/api/v1/auth/session'))['client_id'] == paired['client_id'],
                    'Packaged browser pairing did not establish a session')

            for locale in ('zh-CN', 'en'):
                request = urllib.request.Request(origin + '/api/v1/preferences',
                    data=json.dumps({'locale': locale}).encode(),
                    headers={'Content-Type': 'application/json', 'Origin': origin,
                             'X-CANHarbor-CSRF': paired['csrf_token']}, method='PUT')
                with opener.open(request, timeout=10) as response:
                    require(json.load(response)['locale'] == locale, 'Packaged preference save failed')
                require(f'data-ui-locale="{locale}"' in get('/').decode(),
                        'Packaged HTML locale bootstrap mismatch')
            listener = subprocess.Popen([binary, 'monitor', '--channel', 'virtual:1', '--listen-only', '--count', '1'],
                                        env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding='utf-8')
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                require(listener.poll() is None, 'Packaged monitor exited before acquisition')
                sessions = json.loads(cli('session', 'list'))['sessions']
                if any(s['channel_id'] == 'virtual:1' and s['state'] == 'running' for s in sessions):
                    break
                time.sleep(0.05)
            else:
                raise RuntimeError('Packaged monitor readiness deadline')
            cli('send', '--channel', 'virtual:0', '--id', '123', '--data', '01020304', '--active')
            output, error = listener.communicate(timeout=20)
            require(listener.returncode == 0, f'Packaged monitor failed: {error}')
            frames = [json.loads(line) for line in output.splitlines()]
            require(any(frame['type'] == 'frame_received' and frame['frame']['id'] == 291
                        and frame['frame']['data_hex'] == '01020304' for frame in frames),
                    'Packaged virtual exchange did not receive the submitted frame')
            require(json.loads(cli('session', 'list'))['sessions'] == [], 'Smoke test left CAN sessions open')
            if windows:
                # Attach a separate helper to the service's own console. The CI
                # runner console is untouched; exercise real Tokio Ctrl-C cleanup.
                script = ("import ctypes,sys,time; k=ctypes.WinDLL('kernel32',use_last_error=True); "
                          "k.FreeConsole(); "
                          "assert k.AttachConsole(int(sys.argv[1])), ctypes.get_last_error(); "
                          "assert k.SetConsoleCtrlHandler(None,True); "
                          "assert k.GenerateConsoleCtrlEvent(0,0), ctypes.get_last_error(); time.sleep(0.2)")
                subprocess.run([sys.executable, '-c', script, str(service.pid)], check=True, timeout=10)
            else:
                service.send_signal(signal.SIGINT)
            try:
                code = service.wait(timeout=20)
            except subprocess.TimeoutExpired as error:
                log.seek(0)
                raise RuntimeError('Packaged service shutdown timeout; service log: ' + log.read()[-8000:]) from error
            require(code == 0, f'Packaged service did not shut down cleanly: {code}')
            if not windows:
                require(not (runtime / f'canharbor-{os.geteuid()}' / 'service.sock').exists(),
                        'Packaged service left its socket after shutdown')
            else:
                require(subprocess.run([binary, 'devices'], env=env, capture_output=True, timeout=10).returncode != 0,
                        'Named Pipe remained reachable after shutdown')
        finally:
            cleanup(listener)
            cleanup(service)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--platform', required=True, choices=TARGETS)
    parser.add_argument('--packages-dir', type=pathlib.Path, default=pathlib.Path('dist/packages'))
    args = parser.parse_args()
    host = (platform.system(), platform.machine())
    expected = tuple(TARGETS[args.platform]['host'])
    require(host == expected, f'Run {args.platform} smoke test on its native platform (got {host})')
    repository = pathlib.Path(__file__).resolve().parents[1]
    # A short runtime path avoids the macOS Unix socket limit; extraction under
    # a path containing spaces also verifies the launcher's quoting.
    with tempfile.TemporaryDirectory(prefix='ch-pkg-', dir=None if platform.system() == 'Windows' else '/tmp') as temporary:
        runtime = pathlib.Path(temporary)
        extract = runtime / 'extracted with spaces'
        folder, launcher, version, revision = verify_archives(repository, args.packages_dir.resolve(),
                                                              args.platform, extract)
        smoke(folder, launcher, version, runtime)
    print(json.dumps({'platform': args.platform, 'revision': revision, 'checksums': 'verified',
                      'source': 'matches git', 'architecture': 'verified', 'embedded_web': 'verified',
                      'launcher_and_pairing': 'verified', 'virtual_exchange': 'verified', 'shutdown': 'clean',
                      'windows_runtime': 'system DLLs only' if args.platform == 'windows-x86_64' else 'not applicable'}))


if __name__ == '__main__':
    main()
