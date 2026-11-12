#!/usr/bin/env python3
"""Package a built CLI with committed source without publishing."""
import argparse
import json
import zipfile
import hashlib
import pathlib
import shutil
import subprocess
import tarfile
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    targets = json.loads(pathlib.Path(__file__).with_name('package-targets.json').read_text())
    parser.add_argument('--platform', required=True, choices=targets)
    parser.add_argument('--binary', type=pathlib.Path, default=None)
    args = parser.parse_args()
    root = pathlib.Path(__file__).resolve().parents[1]
    if subprocess.check_output(['git', 'status', '--porcelain', '--untracked-files=normal'], cwd=root):
        parser.error('Package from a clean checkout so source matches the binary.')
    target = targets[args.platform]
    binary = (args.binary or pathlib.Path('target/release') / target['binary']).resolve()
    if not binary.is_file():
        parser.error('Build the release CLI first.')
    revision = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip()
    destination = root / 'dist/packages'
    destination.mkdir(parents=True, exist_ok=True)
    name = f'canharbor-{revision[:12]}-{args.platform}'
    with tempfile.TemporaryDirectory() as temporary:
        folder = pathlib.Path(temporary) / name
        folder.mkdir()
        shutil.copy2(binary, folder / target['binary'])
        launcher = folder / target['launcher']
        if args.platform == 'windows-x86_64':
            launcher.write_bytes((
                '@echo off\r\nsetlocal\r\ncd /d "%~dp0"\r\n'
                'canharbor.exe devices >nul 2>&1\r\nif errorlevel 1 goto start\r\n'
                'if "%~1"=="--print" (canharbor.exe web open --print) else (canharbor.exe web open)\r\n'
                'exit /b %errorlevel%\r\n:start\r\n'
                'if "%~1"=="--print" (echo No running service. Start CANHarbor first. 1>&2 & exit /b 3)\r\n'
                'echo CANHarbor runs in this window. Keep it open while using the workbench.\r\n'
                'canharbor.exe serve --open\r\nexit /b %errorlevel%\r\n').encode('ascii'))
        else:
            launcher.write_text('#!/bin/sh\nset -eu\ncd "$(dirname "$0")"\n'
                                '# Reuse an existing local service instead of creating a competing one.\n'
                                'if ./canharbor devices >/dev/null 2>&1; then\n'
                                '  exec ./canharbor web open\nfi\n'
                                'printf "%s\\n" "CANHarbor runs in this window. Keep it open while using the workbench." "CANHarbor 在此窗口中运行。使用工作台期间请保持窗口打开。"\n'
                                'exec ./canharbor serve --open\n', encoding='utf-8', newline='\n')
        launcher.chmod(0o755)
        for item in ['LICENSE', 'THIRD_PARTY_NOTICES', 'LICENSES']:
            source = root / item
            if source.is_dir():
                shutil.copytree(source, folder / item)
            else:
                shutil.copy2(source, folder / item)
        (folder / 'START.txt').write_text('Start: ' + launcher.name + ' (macOS: double-click in Finder).\n'
            'Windows: canharbor.exe serve --open; macOS/Linux: ./canharbor serve --open. Keep that window open while using the workbench.\n'
            'Reopening the launcher pairs another browser with an existing default service.\n'
            'For a custom runtime directory, use the CLI and pass --runtime-dir explicitly.\n'
            'First receive/send walkthrough: open the guide in the connection panel.\n\n'
            'Default: two simulated channels. Hardware compatibility is unverified.\n'
            '''This binary is unsigned and not notarized.

启动：''' + launcher.name + '''（macOS 可在 Finder 中双击）。
也可在终端运行 ./canharbor serve --open。使用工作台期间请保持该窗口打开。
重复打开启动器会使用已有的默认服务，为另一个浏览器配对。
自定义运行目录时，请使用 CLI 并明确传入 --runtime-dir。
界面顶部可选择自动、English 或简体中文；切换语言不会启动通道或发送帧。
命令行帮助：./canharbor --lang zh-CN --help
默认提供两个模拟通道，硬件兼容性尚未验证。此二进制未签名、未公证。

Source revision: ''' + revision + '\n', encoding='utf-8', newline='\n')
        archive = destination / (name + target['extension'])
        if target['extension'] == '.zip':
            with zipfile.ZipFile(archive, 'w', compression=zipfile.ZIP_DEFLATED) as output:
                for path in sorted(folder.rglob('*')):
                    output.write(path, str(path.relative_to(folder.parent)).replace('\\', '/'))
        else:
            with tarfile.open(archive, 'w:gz') as output:
                def unix_mode(entry):
                    entry.mode = 0o755 if entry.isdir() or entry.name in [name + '/canharbor', name + '/' + launcher.name] else 0o644
                    return entry
                output.add(folder, arcname=name, filter=unix_mode)
    source_archive = destination / f'canharbor-{revision[:12]}-source.tar.gz'
    subprocess.run(['git', 'archive', '--format=tar.gz', '--prefix=canharbor-source/',
                    '-o', str(source_archive), revision], cwd=root, check=True)
    for archive in [archive, source_archive]:
        with archive.open('rb') as stream:
            digest = hashlib.file_digest(stream, 'sha256').hexdigest()
        archive.with_suffix(archive.suffix + '.sha256').write_text(f'{digest}  {archive.name}\n', encoding='ascii', newline='\n')
        print(archive)


if __name__ == '__main__':
    main()
