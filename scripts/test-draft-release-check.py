#!/usr/bin/env python3
"""Regression tests for draft asset integrity and narrowly scoped cleanup."""
import copy
import hashlib
import importlib.util
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile
import unittest


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


check = load('draft_release_check', 'draft-release-check.py')
fixtures = load('package_tests', 'test-package.py')


class FakeGitHub:
    def __init__(self, context):
        self.context = context
        self.ref = None
        self.tags = {}
        self.releases = []
        self.events = []

    def api(self, method, endpoint, data=None, missing=False):
        self.events.append((method, endpoint, copy.deepcopy(data)))
        base = self.context.base
        if method == 'GET' and endpoint == f'{base}/git/ref/tags/{self.context.tag}':
            return copy.deepcopy(self.ref)
        if method == 'GET' and endpoint.startswith(f'{base}/releases?'):
            page = int(endpoint.rsplit('page=', 1)[1])
            return copy.deepcopy(self.releases[(page - 1) * 100:page * 100])
        if method == 'POST' and endpoint == f'{base}/git/tags':
            sha = 'b' * 40
            self.tags[sha] = {'sha': sha, 'tag': data['tag'], 'message': data['message'],
                              'object': {'type': data['type'], 'sha': data['object']}}
            return copy.deepcopy(self.tags[sha])
        if method == 'GET' and endpoint.startswith(f'{base}/git/tags/'):
            return copy.deepcopy(self.tags[endpoint.rsplit('/', 1)[1]])
        if method == 'POST' and endpoint == f'{base}/git/refs':
            self.ref = {'ref': data['ref'], 'object': {'type': 'tag', 'sha': data['sha']}}
            return copy.deepcopy(self.ref)
        if method == 'DELETE' and endpoint.startswith(f'{base}/releases/'):
            self.releases = [r for r in self.releases if str(r['id']) != endpoint.rsplit('/', 1)[1]]
            return None
        if method == 'DELETE' and endpoint == f'{base}/git/refs/tags/{self.context.tag}':
            self.ref = None
            return None
        raise AssertionError((method, endpoint, data))

    def create(self):
        def emit(values):
            self.events.append(('OUTPUT', None, values.copy()))
        return check.create_tag(self.context, self.api, emit)['tag_object']

    def draft(self):
        self.releases.append({'id': 7, 'tag_name': self.context.tag, 'draft': True, 'published_at': None})


class LifecycleTest(unittest.TestCase):
    def setUp(self):
        self.context = check.Context('owner/repo', 'a' * 40, '1234', '2')
        self.github = FakeGitHub(self.context)

    def mutations(self):
        return [event for event in self.github.events if event[0] in ['POST', 'DELETE']]

    def test_ownership_recorded_before_visible_ref(self):
        self.github.create()
        kinds = [(method, endpoint) for method, endpoint, _ in self.github.events]
        self.assertLess(kinds.index(('OUTPUT', None)), kinds.index(('POST', self.context.base + '/git/refs')))

    def test_cleanup_after_success_or_verification_failure(self):
        owned = self.github.create()
        self.github.draft()
        self.github.releases.append({'id': 8, 'tag_name': 'v1.0.0', 'draft': False, 'published_at': 'date'})
        check.cleanup(self.context, owned, self.github.api)
        self.assertIsNone(self.github.ref)
        self.assertEqual([r['tag_name'] for r in self.github.releases], ['v1.0.0'])
        self.assertEqual([event[1] for event in self.github.events if event[0] == 'DELETE'],
                         [self.context.base + '/releases/7', self.context.base + '/git/refs/tags/' + self.context.tag])

    def test_cleanup_after_build_failure_before_draft(self):
        owned = self.github.create()
        check.cleanup(self.context, owned, self.github.api)
        self.assertIsNone(self.github.ref)

    def test_cleanup_is_idempotent_after_tag_object_collection(self):
        owned = self.github.create()
        self.github.draft()
        check.cleanup(self.context, owned, self.github.api)
        self.github.tags.clear()
        check.cleanup(self.context, owned, self.github.api)

    def test_published_release_is_never_deleted(self):
        owned = self.github.create()
        self.github.draft()
        self.github.releases[0].update(draft=False, published_at='date')
        self.github.events.clear()
        with self.assertRaisesRegex(RuntimeError, 'unpublished draft'):
            check.cleanup(self.context, owned, self.github.api)
        self.assertEqual(self.mutations(), [])

    def test_moved_tag_is_never_deleted(self):
        owned = self.github.create()
        self.github.ref['object']['sha'] = 'c' * 40
        self.github.draft()
        self.github.events.clear()
        with self.assertRaisesRegex(RuntimeError, 'tag moved'):
            check.cleanup(self.context, owned, self.github.api)
        self.assertEqual(self.mutations(), [])

    def test_changed_ownership_marker_is_rejected(self):
        owned = self.github.create()
        self.github.tags[owned]['message'] = 'another run'
        self.github.events.clear()
        with self.assertRaisesRegex(RuntimeError, 'does not belong'):
            check.cleanup(self.context, owned, self.github.api)
        self.assertEqual(self.mutations(), [])

    def test_preexisting_tag_is_not_adopted_or_cleaned(self):
        self.github.ref = {'ref': 'existing', 'object': {'sha': 'c' * 40}}
        with self.assertRaisesRegex(RuntimeError, 'already exists'):
            self.github.create()
        check.cleanup(self.context, '', self.github.api)
        self.assertEqual(self.mutations(), [])

    def test_preexisting_release_is_not_adopted(self):
        self.github.draft()
        with self.assertRaisesRegex(RuntimeError, 'already exists'):
            self.github.create()
        self.assertEqual(self.mutations(), [])

    def test_release_lookup_paginates(self):
        self.github.releases = [{'tag_name': 'other'} for _ in range(100)]
        self.github.draft()
        self.assertEqual(check.find_release(self.context, self.github.api)['id'], 7)

    def test_ref_created_then_api_response_lost_can_be_cleaned(self):
        outputs = {}
        def api(method, endpoint, data=None, missing=False):
            result = self.github.api(method, endpoint, data, missing)
            if method == 'POST' and endpoint.endswith('/git/refs'):
                raise RuntimeError('response lost after commit')
            return result
        with self.assertRaisesRegex(RuntimeError, 'response lost'):
            check.create_tag(self.context, api, outputs.update)
        check.cleanup(self.context, outputs['tag_object'], self.github.api)
        self.assertIsNone(self.github.ref)


class AssetsTest(unittest.TestCase):
    def setUp(self):
        fixture = fixtures.VerificationTest('test_valid_archives')
        self.addCleanup(fixture.doCleanups)
        fixture.setUp()
        self.fixture = fixture
        self.temporary = tempfile.TemporaryDirectory(prefix='ch-draft-assets-')
        self.addCleanup(self.temporary.cleanup)
        self.data = Path(self.temporary.name)
        self.artifacts = self.data / 'artifacts'
        self.artifacts.mkdir()
        shutil.copytree(fixture.packages, self.artifacts / 'canharbor-linux-x86_64')
        for path in fixture.packages.iterdir():
            path.unlink()
        header = bytearray(64)
        struct.pack_into('<II', header, 0, 0xfeedfacf, 0x0100000c)
        (fixture.root / 'target/release/canharbor').write_bytes(header)
        subprocess.run([sys.executable, 'scripts/package.py', '--platform', 'macos-arm64'],
                       cwd=fixture.root, check=True, capture_output=True)
        shutil.copytree(fixture.packages, self.artifacts / 'canharbor-macos-arm64')
        for path in fixture.packages.iterdir():
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
        (fixture.root / 'target/release/canharbor.exe').write_bytes(header)
        subprocess.run([sys.executable, 'scripts/package.py', '--platform', 'windows-x86_64'],
                       cwd=fixture.root, check=True, capture_output=True)
        shutil.copytree(fixture.packages, self.artifacts / 'canharbor-windows-x86_64')
        self.downloaded = self.data / 'release'
        self.downloaded.mkdir()
        for artifact in self.artifacts.iterdir():
            for path in artifact.iterdir():
                shutil.copy2(path, self.downloaded / path.name)
        revision = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=fixture.root, text=True).strip()
        self.context = check.Context('owner/repo', revision, '1234', '1')
        self.release = {'tag_name': self.context.tag, 'draft': True, 'published_at': None, 'assets': []}
        self.refresh_metadata()

    def refresh_metadata(self):
        self.release['assets'] = [{'name': path.name, 'size': path.stat().st_size, 'state': 'uploaded',
                                  'digest': 'sha256:' + hashlib.sha256(path.read_bytes()).hexdigest()}
                                 for path in self.downloaded.iterdir()]

    def verify(self):
        return check.verify_assets(self.context, self.release, self.fixture.root, self.downloaded, self.artifacts)

    def test_complete_assets(self):
        self.assertEqual(len(self.verify()['assets']), 8)

    def test_missing_remote_asset(self):
        self.release['assets'].pop()
        with self.assertRaisesRegex(RuntimeError, 'exactly three platform archives'):
            self.verify()

    def test_missing_downloaded_checksum(self):
        next(self.downloaded.glob('*.sha256')).unlink()
        with self.assertRaisesRegex(RuntimeError, 'Downloaded release inventory'):
            self.verify()

    def test_asset_digest_mismatch(self):
        path = next(self.downloaded.glob('*.sha256'))
        data = bytearray(path.read_bytes())
        data[0] = ord('0') if data[0] != ord('0') else ord('1')
        path.write_bytes(data)
        with self.assertRaisesRegex(RuntimeError, 'Asset digest mismatch'):
            self.verify()

    def test_artifact_identity_mismatch(self):
        path = next((self.artifacts / 'canharbor-linux-x86_64').glob('*linux-x86_64.tar.gz'))
        path.write_bytes(b'other binary')
        with self.assertRaisesRegex(RuntimeError, 'differs from tested workflow artifact'):
            self.verify()

    def test_forged_source_with_updated_digests_is_rejected(self):
        def change(entry, content):
            return entry, b'changed source' if entry.name.endswith('/LICENSE') else content
        self.fixture.rewrite(self.fixture.source, change)
        for path in [self.fixture.source, self.fixture.source.with_suffix('.gz.sha256')]:
            shutil.copy2(path, self.downloaded / path.name)
            for artifact in self.artifacts.iterdir():
                shutil.copy2(path, artifact / path.name)
        self.refresh_metadata()
        with self.assertRaisesRegex(RuntimeError, 'Source content differs'):
            self.verify()

    def test_published_release_is_rejected(self):
        self.release.update(draft=False, published_at='date')
        with self.assertRaisesRegex(RuntimeError, 'unpublished draft'):
            self.verify()


if __name__ == '__main__':
    unittest.main()
