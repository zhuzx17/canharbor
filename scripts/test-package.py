#!/usr/bin/env python3
"""Exercise package/source identity and launchers in an isolated clean checkout."""
import hashlib
import copy
import importlib.util
import io
import os
from pathlib import Path
import shutil
import subprocess
import struct
import sys
import tarfile
import tempfile
import unittest
import zipfile

spec = importlib.util.spec_from_file_location('verify_package', Path(__file__).with_name('verify-package.py'))
verify_package = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verify_package)


class PackageTest(unittest.TestCase):
    @unittest.skipIf(os.name == "nt", "POSIX launcher execution; Windows ZIP/launcher tests run separately")
    def test_archives_and_launchers(self):
        repository = Path(__file__).resolve().parents[1]
        for platform in ("macos-arm64", "linux-x86_64"):
            with self.subTest(platform=platform), tempfile.TemporaryDirectory(prefix="canharbor package ") as temporary:
                root = Path(temporary)
                (root / "scripts").mkdir()
                shutil.copy2(repository / "scripts/package.py", root / "scripts/package.py")
                shutil.copy2(repository / "scripts/package-targets.json", root / "scripts/package-targets.json")
                for name in ("LICENSE", "THIRD_PARTY_NOTICES"):
                    shutil.copy2(repository / name, root / name)
                shutil.copytree(repository / "LICENSES", root / "LICENSES")
                (root / ".gitignore").write_text("/target/\n/dist/\n")
                for command in (["git", "init", "-q"], ["git", "config", "core.autocrlf", "false"], ["git", "add", "."],
                                ["git", "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", "-c", "user.name=Package Test", "-c", "user.email=package-test@example.invalid", "commit", "-qm", "fixture"]):
                    subprocess.run(command, cwd=root, check=True, capture_output=True)
                binary = root / "target/release/canharbor"
                binary.parent.mkdir(parents=True)
                binary.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$CANHARBOR_LAUNCH_LOG"\n'
                                  'if [ "$1" = devices ]; then exit "$CANHARBOR_SERVICE_EXISTS"; fi\n')
                binary.chmod(0o755)
                subprocess.run(["python3", "scripts/package.py", "--platform", platform], cwd=root, check=True, capture_output=True)
                packages = root / "dist/packages"
                archives = list(packages.glob("*.tar.gz"))
                self.assertEqual(len(archives), 2)
                for archive in archives:
                    checksum = archive.with_suffix(".gz.sha256").read_text().split()[0]
                    self.assertEqual(checksum, hashlib.sha256(archive.read_bytes()).hexdigest())
                binary_archive = next(path for path in archives if platform in path.name)
                extract = root / "extracted with spaces"
                with tarfile.open(binary_archive) as archive:
                    archive.extractall(extract, filter="data")
                folder = next(extract.iterdir())
                revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
                self.assertIn("Source revision: " + revision, (folder / "START.txt").read_text())
                self.assertIn(revision[:12], binary_archive.name)
                with tarfile.open(next(path for path in archives if "source" in path.name)) as source:
                    self.assertIn("canharbor-source/scripts/package.py", source.getnames())
                launcher = folder / ("Start CANHarbor.command" if platform == "macos-arm64" else "start-canharbor.sh")
                self.assertTrue(os.access(launcher, os.X_OK))
                log = root / "launcher.log"
                env = {**os.environ, "CANHARBOR_LAUNCH_LOG": str(log)}
                for service_exists, expected in (("1", ["devices", "serve --open"]), ("0", ["devices", "web open"])):
                    log.unlink(missing_ok=True)
                    subprocess.run([str(launcher)], cwd="/tmp", env={**env, "CANHARBOR_SERVICE_EXISTS": service_exists}, check=True, capture_output=True)
                    self.assertEqual(log.read_text().splitlines(), expected)
                (root / "uncommitted.txt").write_text("dirty")
                rejected = subprocess.run(["python3", "scripts/package.py", "--platform", platform], cwd=root, capture_output=True)
                self.assertNotEqual(rejected.returncode, 0)


class VerificationTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='ch-package-check-')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / 'scripts').mkdir()
        shutil.copy2(Path(__file__).with_name('package.py'), self.root / 'scripts/package.py')
        shutil.copy2(Path(__file__).with_name('package-targets.json'), self.root / 'scripts/package-targets.json')
        (self.root / 'Cargo.toml').write_text('[workspace.package]\nversion = "0.1.0-test"\n')
        (self.root / 'LICENSE').write_text('license fixture\n')
        (self.root / 'THIRD_PARTY_NOTICES').write_text('notices fixture\n')
        (self.root / 'LICENSES').mkdir()
        (self.root / 'LICENSES/test.txt').write_text('dependency license fixture\n')
        (self.root / '.gitignore').write_text('/target/\n/dist/\n')
        for command in (["git", "init", "-q"], ["git", "config", "core.autocrlf", "false"], ["git", "add", "."],
                        ["git", "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null",
                         "-c", "user.name=Package Test", "-c", "user.email=package-test@example.invalid",
                         "commit", "-qm", "fixture"]):
            subprocess.run(command, cwd=self.root, check=True, capture_output=True)
        binary = self.root / 'target/release/canharbor'
        binary.parent.mkdir(parents=True)
        # Static archive checks need a target header, not an executable mock of
        # the service. The workflow separately runs its real extracted binary.
        header = bytearray(64)
        header[:6] = b'\x7fELF\x02\x01'
        struct.pack_into('<H', header, 18, 62)
        binary.write_bytes(header)
        binary.chmod(0o755)
        subprocess.run([sys.executable, 'scripts/package.py', '--platform', 'linux-x86_64'],
                       cwd=self.root, check=True, capture_output=True)
        self.packages = self.root / 'dist/packages'
        self.archive = next(self.packages.glob('*-linux-x86_64.tar.gz'))
        self.source = next(self.packages.glob('*-source.tar.gz'))

    def verify(self):
        return verify_package.verify_archives(self.root, self.packages, 'linux-x86_64', self.root / 'extract')

    def rewrite(self, archive, transform):
        with tarfile.open(archive) as source:
            entries = [(copy.copy(entry), source.extractfile(entry).read() if entry.isfile() else None)
                       for entry in source.getmembers()]
        with tarfile.open(archive, 'w:gz') as output:
            for entry, content in entries:
                entry, content = transform(entry, content)
                if content is not None:
                    entry.size = len(content)
                output.addfile(entry, io.BytesIO(content) if content is not None else None)
        archive.with_suffix('.gz.sha256').write_text(
            f'{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n')

    def test_valid_archives(self):
        folder, launcher, version, revision = self.verify()
        self.assertTrue((folder / launcher).is_file())
        self.assertEqual(version, '0.1.0-test')
        self.assertIn(revision[:12], folder.name)

    def test_bad_checksum(self):
        self.archive.with_suffix('.gz.sha256').write_text(f'{"0" * 64}  {self.archive.name}\n')
        with self.assertRaisesRegex(RuntimeError, 'SHA-256 mismatch'):
            self.verify()

    def test_changed_source_even_with_updated_checksum(self):
        def change(entry, content):
            return entry, b'tampered source\n' if entry.name.endswith('/LICENSE') else content
        self.rewrite(self.source, change)
        with self.assertRaisesRegex(RuntimeError, 'Source content differs'):
            self.verify()

    def test_changed_source_mode(self):
        def change(entry, content):
            if entry.name.endswith('/LICENSE'):
                entry.mode = 0o755
            return entry, content
        self.rewrite(self.source, change)
        with self.assertRaisesRegex(RuntimeError, 'Source metadata differs'):
            self.verify()

    def test_wrong_architecture(self):
        def change(entry, content):
            if entry.name.endswith('/canharbor'):
                header = bytearray(content)
                struct.pack_into('<H', header, 18, 183)
                content = bytes(header)
            return entry, content
        self.rewrite(self.archive, change)
        with self.assertRaisesRegex(RuntimeError, 'Expected an ELF64 x86-64'):
            self.verify()

    def test_unsafe_binary_path(self):
        def change(entry, content):
            if entry.name.endswith('/START.txt'):
                entry.name = '../escape'
            return entry, content
        self.rewrite(self.archive, change)
        with self.assertRaisesRegex(RuntimeError, 'Unsafe binary archive member'):
            self.verify()
        self.assertFalse((self.root / 'escape').exists())

    def test_non_executable_launcher(self):
        def change(entry, content):
            if entry.name.endswith('/start-canharbor.sh'):
                entry.mode = 0o644
            return entry, content
        self.rewrite(self.archive, change)
        with self.assertRaisesRegex(RuntimeError, 'not executable'):
            self.verify()

    def test_stale_artifact(self):
        (self.packages / 'stale.tar.gz').write_bytes(b'old revision')
        with self.assertRaisesRegex(RuntimeError, 'exactly this revision/platform'):
            self.verify()


class WindowsVerificationTest(unittest.TestCase):
    def setUp(self):
        self.fixture = VerificationTest('test_valid_archives')
        self.addCleanup(self.fixture.doCleanups)
        self.fixture.setUp()
        self.root, self.packages = self.fixture.root, self.fixture.packages
        for path in self.packages.iterdir():
            path.unlink()
        header = bytearray(512)
        header[:2] = b'MZ'
        struct.pack_into('<I', header, 60, 128)
        header[128:132] = b'PE\0\0'
        struct.pack_into('<H', header, 132, 0x8664)
        struct.pack_into('<H', header, 148, 240)
        struct.pack_into('<H', header, 152, 0x20b)
        struct.pack_into('<I', header, 212, 512)
        struct.pack_into('<I', header, 260, 16)
        (self.root / 'target/release/canharbor.exe').write_bytes(header)
        subprocess.run([sys.executable, 'scripts/package.py', '--platform', 'windows-x86_64'],
                       cwd=self.root, check=True, capture_output=True)
        self.archive = next(self.packages.glob('*.zip'))

    def verify(self):
        return verify_package.verify_archives(self.root, self.packages, 'windows-x86_64', self.root / 'windows extract')

    def rewrite(self, transform):
        with zipfile.ZipFile(self.archive) as source:
            entries = [(copy.copy(i), source.read(i)) for i in source.infolist()]
        with zipfile.ZipFile(self.archive, 'w') as target:
            for entry, data in entries:
                entry, data = transform(entry, data)
                target.writestr(entry, data)
        self.archive.with_suffix('.zip.sha256').write_text(
            f'{hashlib.sha256(self.archive.read_bytes()).hexdigest()}  {self.archive.name}\n')

    def test_windows_archive_and_launcher(self):
        folder, launcher, _, _ = self.verify()
        self.assertEqual(launcher, 'start-canharbor.cmd')
        self.assertTrue((folder / 'canharbor.exe').is_file())
        self.assertIn(b'cd /d "%~dp0"', (folder / launcher).read_bytes())
        self.assertIn(b'web open --print', (folder / launcher).read_bytes())

    def test_runtime_dependency_audit(self):
        data = bytearray(1024)
        data[:2] = b'MZ'
        struct.pack_into('<I', data, 60, 128)
        data[128:132] = b'PE\0\0'
        struct.pack_into('<H', data, 134, 1)
        struct.pack_into('<H', data, 148, 240)
        struct.pack_into('<H', data, 152, 0x20b)
        struct.pack_into('<I', data, 212, 512)
        struct.pack_into('<I', data, 260, 16)
        struct.pack_into('<II', data, 272, 0x1000, 40)
        struct.pack_into('<IIII', data, 400, 256, 0x1000, 256, 512)
        struct.pack_into('<IIIII', data, 512, 1, 0, 0, 0x1040, 1)
        for name in [b'kernel32.dll', b'VCRUNTIME140.dll', b'custom.dll']:
            data[576:836] = bytes(260)
            data[576:576+len(name)] = name
            if name == b'kernel32.dll':
                self.assertEqual(verify_package.verify_windows_dependencies(data), ['kernel32.dll'])
            else:
                with self.assertRaisesRegex(RuntimeError, 'Non-system Windows runtime DLLs'):
                    verify_package.verify_windows_dependencies(data)
        struct.pack_into('<I', data, 524, 0xffffffff)
        with self.assertRaisesRegex(RuntimeError, 'Invalid PE import RVA'):
            verify_package.windows_imports(data)

    def test_wrong_windows_architecture(self):
        self.rewrite(lambda i, b: (i, b'not a PE' if i.filename.endswith('.exe') else b))
        with self.assertRaisesRegex(RuntimeError, 'PE32'):
            self.verify()

    def test_zip_traversal(self):
        def change(info, data):
            if info.filename.endswith('.exe'):
                info.filename = '../escape.exe'
            return info, data
        self.rewrite(change)
        with self.assertRaisesRegex(RuntimeError, 'Unsafe binary archive member'):
            self.verify()
        self.assertFalse((self.root / 'escape.exe').exists())

    def test_zip_symlink(self):
        def change(info, data):
            if info.filename.endswith('.exe'):
                info.external_attr = 0o120777 << 16
            return info, data
        self.rewrite(change)
        with self.assertRaisesRegex(RuntimeError, 'Unsafe ZIP entry type'):
            self.verify()


if __name__ == "__main__":
    unittest.main()
