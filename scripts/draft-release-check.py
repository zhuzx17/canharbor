#!/usr/bin/env python3
"""Create, verify and clean up a run-owned draft-release rehearsal. Never publish."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

spec = importlib.util.spec_from_file_location('verify_package', Path(__file__).with_name('verify-package.py'))
verify_package = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verify_package)
require = verify_package.require


class Context:
    def __init__(self, repository, revision, run_id, attempt):
        require(re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', repository), 'Invalid repository')
        require(re.fullmatch(r'[0-9a-f]{40}', revision), 'Expected a full commit SHA')
        require(re.fullmatch(r'[1-9][0-9]*', run_id) and re.fullmatch(r'[1-9][0-9]*', attempt),
                'Expected workflow run ID and attempt')
        self.repository, self.revision = repository, revision
        self.base = f'repos/{repository}'
        self.tag = f'v0.0.0-draft-check-{run_id}-{attempt}'
        self.marker = (f'CANHarbor draft rehearsal\nrepository: {repository}\n'
                       f'run: {run_id}\nattempt: {attempt}\ncommit: {revision}\n')


def gh_api(method, endpoint, data=None, missing=False):
    command = ['gh', 'api', '--method', method, endpoint]
    if data is not None:
        command += ['--input', '-']
    result = subprocess.run(command, input=json.dumps(data) if data is not None else None,
                            capture_output=True, text=True, timeout=60)
    if result.returncode:
        if missing and method == 'GET' and '(HTTP 404)' in result.stderr:
            return None
        raise RuntimeError(f'{method} {endpoint}: {result.stderr.strip()}')
    return json.loads(result.stdout) if result.stdout.strip() else None


def find_release(context, api):
    found = []
    page = 1
    while True:
        releases = api('GET', f'{context.base}/releases?per_page=100&page={page}')
        found.extend(release for release in releases if release['tag_name'] == context.tag)
        if len(releases) < 100:
            break
        page += 1
    require(len(found) <= 1, 'Ambiguous releases for rehearsal tag')
    return found[0] if found else None


def write_outputs(values):
    with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
        for name, value in values.items():
            require('\n' not in value and '\r' not in value, 'Invalid workflow output')
            output.write(f'{name}={value}\n')


def create_tag(context, api=gh_api, emit=write_outputs):
    require(api('GET', f'{context.base}/git/ref/tags/{context.tag}', missing=True) is None,
            'Rehearsal tag already exists; refusing to adopt it')
    require(find_release(context, api) is None, 'Rehearsal release already exists; refusing to adopt it')
    tag = api('POST', f'{context.base}/git/tags', {
        'tag': context.tag, 'message': context.marker, 'object': context.revision, 'type': 'commit',
    })
    owned = tag['sha']
    require(re.fullmatch(r'[0-9a-f]{40}', owned), 'Invalid created tag object SHA')
    # Record the exact object before creating a visible ref. Even an API error
    # after the ref is committed leaves cleanup enough ownership information.
    emit({'tag': context.tag, 'tag_object': owned})
    api('POST', f'{context.base}/git/refs', {'ref': f'refs/tags/{context.tag}', 'sha': owned})
    return {'tag': context.tag, 'tag_object': owned, 'revision': context.revision}


def owned_ref(context, owned, api):
    require(re.fullmatch(r'[0-9a-f]{40}', owned or ''), 'Missing run-owned tag object')
    tag = api('GET', f'{context.base}/git/tags/{owned}')
    require(tag['sha'] == owned and tag['tag'] == context.tag and tag['message'] == context.marker
            and tag['object']['type'] == 'commit' and tag['object']['sha'] == context.revision,
            'Tag object does not belong to this rehearsal')
    ref = api('GET', f'{context.base}/git/ref/tags/{context.tag}', missing=True)
    if ref is not None:
        require(ref['ref'] == f'refs/tags/{context.tag}' and ref['object']['type'] == 'tag'
                and ref['object']['sha'] == owned, 'Rehearsal tag moved; refusing to delete it')
    return ref


def unpublished(context, release):
    require(release is not None and release['tag_name'] == context.tag
            and release['draft'] is True and release['published_at'] is None,
            'Expected this rehearsal\'s unpublished draft; refusing a published release')


def inventory(context, release):
    unpublished(context, release)
    prefix = f'canharbor-{context.revision[:12]}'
    archives = [f'{prefix}-{target}' + spec['extension'] for target, spec in verify_package.TARGETS.items()]
    archives.append(f'{prefix}-source.tar.gz')
    expected = set(archives + [name + '.sha256' for name in archives])
    require(len(release['assets']) == len(expected) and {asset['name'] for asset in release['assets']} == expected,
            'Draft must contain exactly three platform archives, source and four checksums')
    return expected


def verify_assets(context, release, repository, downloaded, artifacts):
    expected = inventory(context, release)
    head = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=repository, text=True).strip()
    require(head == context.revision, 'Verification checkout differs from the tagged commit')
    prefix = f'canharbor-{context.revision[:12]}'
    platforms = list(verify_package.TARGETS)
    assets = release['assets']
    require({path.name for path in downloaded.iterdir()} == expected, 'Downloaded release inventory differs')
    for asset in assets:
        path = downloaded / asset['name']
        require(path.is_file() and not path.is_symlink() and asset['state'] == 'uploaded'
                and path.stat().st_size == asset['size'] and asset['size'] > 0,
                f'Incomplete release asset: {asset["name"]}')
        with path.open('rb') as stream:
            digest = hashlib.file_digest(stream, 'sha256').hexdigest()
        require(asset.get('digest') == 'sha256:' + digest, f'Asset digest mismatch: {asset["name"]}')
    require({path.name for path in artifacts.iterdir()} == {'canharbor-' + target for target in platforms},
            'Expected exactly the three platform workflow artifacts')
    with tempfile.TemporaryDirectory(prefix='ch-release-') as temporary:
        for target in platforms:
            names = [f'{prefix}-{target}' + verify_package.TARGETS[target]['extension'], f'{prefix}-source.tar.gz']
            names += [name + '.sha256' for name in names.copy()]
            artifact = artifacts / ('canharbor-' + target)
            require({path.name for path in artifact.iterdir()} == set(names), 'Workflow artifact inventory differs')
            package = Path(temporary) / target
            package.mkdir()
            for name in names:
                require((artifact / name).is_file() and not (artifact / name).is_symlink(), 'Invalid artifact file')
                require((artifact / name).read_bytes() == (downloaded / name).read_bytes(),
                        f'Draft differs from tested workflow artifact: {name}')
                shutil.copyfile(downloaded / name, package / name)
            # Static archive verification works for either target on this
            # runner. Native startup tests already ran in the called matrix.
            verify_package.verify_archives(repository, package, target, Path(temporary) / ('extract-' + target))
    return {'tag': context.tag, 'revision': context.revision, 'draft': True, 'assets': sorted(expected),
            'checksums_source_architecture_and_artifact_identity': 'verified'}


def verify(context, owned, repository, artifacts, api=gh_api):
    require(owned_ref(context, owned, api) is not None, 'Rehearsal tag disappeared before verification')
    release = find_release(context, api)
    inventory(context, release)
    with tempfile.TemporaryDirectory(prefix='ch-draft-download-') as temporary:
        downloaded = Path(temporary)
        subprocess.run(['gh', 'release', 'download', context.tag, '--repo', context.repository,
                        '--dir', str(downloaded)], check=True, timeout=120)
        return verify_assets(context, release, repository, downloaded, artifacts)


def cleanup(context, owned, api=gh_api):
    if not owned:
        return {'tag': context.tag, 'cleanup': 'nothing owned; no mutations'}
    require(re.fullmatch(r'[0-9a-f]{40}', owned), 'Invalid run-owned tag object')
    # An already-clean run must not depend on an unreferenced tag object still
    # being retained by GitHub's object storage.
    if (api('GET', f'{context.base}/git/ref/tags/{context.tag}', missing=True) is None
            and find_release(context, api) is None):
        return {'tag': context.tag, 'cleanup': 'draft and tag confirmed absent'}
    owned_ref(context, owned, api)
    release = find_release(context, api)
    if release is not None:
        unpublished(context, release)
        api('DELETE', f'{context.base}/releases/{release["id"]}')
    # Recheck ownership immediately before removing the ref. Do not delete a
    # tag moved by someone else, or remove any formally published release.
    if owned_ref(context, owned, api) is not None:
        api('DELETE', f'{context.base}/git/refs/tags/{context.tag}')
    require(find_release(context, api) is None, 'Test release remains after cleanup')
    require(api('GET', f'{context.base}/git/ref/tags/{context.tag}', missing=True) is None,
            'Test tag remains after cleanup')
    return {'tag': context.tag, 'cleanup': 'draft and tag confirmed absent'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('operation', choices=['create', 'verify', 'cleanup'])
    parser.add_argument('--artifacts-dir', type=Path, default=Path('workflow-artifacts'))
    args = parser.parse_args()
    context = Context(os.environ['GITHUB_REPOSITORY'], os.environ['GITHUB_SHA'],
                      os.environ['GITHUB_RUN_ID'], os.environ['GITHUB_RUN_ATTEMPT'])
    owned = os.environ.get('OWNED_TAG_OBJECT', '')
    if args.operation == 'create':
        result = create_tag(context)
    elif args.operation == 'verify':
        result = verify(context, owned, Path(__file__).resolve().parents[1], args.artifacts_dir)
    else:
        result = cleanup(context, owned)
    print(json.dumps(result))
    if os.environ.get('GITHUB_STEP_SUMMARY'):
        with open(os.environ['GITHUB_STEP_SUMMARY'], 'a') as summary:
            summary.write(f'### Draft rehearsal: {args.operation}\n\n```json\n{json.dumps(result, indent=2)}\n```\n')


if __name__ == '__main__':
    main()
