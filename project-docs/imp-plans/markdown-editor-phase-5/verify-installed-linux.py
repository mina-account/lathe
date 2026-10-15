import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time

source = Path(__file__).resolve().parents[3]
archive = Path(sys.argv[1]).resolve()
root = Path(sys.argv[2]) if len(sys.argv) > 2 else Path('/home/an/.cache/lathe-phase5-installed-linux')
installation = root / 'application'
environment = os.environ.copy()
environment.update(PATH='/usr/bin:/bin', LATHE_ARCHIVE_PATH=str(archive), LATHE_INSTALL_ROOT=str(installation), LATHE_INSTALL_NO_SHELL='1')
subprocess.run(['bash', str(source / 'script/install-fork-linux')], env=environment, check=True)
executable = installation / 'libexec/lathe-editor'
for key in ['CEF_PATH', 'LATHE_MARKDOWN_CEF_RUNTIME', 'LD_LIBRARY_PATH', 'LD_PRELOAD', 'NO_PROXY', 'no_proxy']:
    environment.pop(key, None)
environment.update(VK_ICD_FILENAMES='/usr/share/vulkan/icd.d/lvp_icd.json')
dependencies = subprocess.check_output(['/usr/bin/ldd', str(executable)], env=environment, text=True)
assert 'not found' not in dependencies, dependencies
selected_dependencies = dict(re.findall(r'^\s*(\S+) => (/\S+)', dependencies, re.MULTILINE))
bundled_dependencies = {}
for name, selected in selected_dependencies.items():
    for directory in [installation / 'libexec', installation / 'lib']:
        bundled = directory / name
        if bundled.is_file():
            assert Path(selected).resolve() == bundled.resolve(), f'{name}: selected {selected} instead of bundled {bundled}'
            bundled_dependencies[name] = {'path': str(bundled), 'sha256': hashlib.sha256(bundled.read_bytes()).hexdigest()}
            break
for name in ['libcef.so', 'libnspr4.so', 'libnss3.so', 'libnssutil3.so', 'libsmime3.so']:
    assert name in bundled_dependencies, f'{name} is not loaded from the package'
for key in ['HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY', 'http_proxy', 'https_proxy', 'all_proxy']:
    environment[key] = 'http://127.0.0.1:9'
assert shutil.which('node', path=environment['PATH']) is None
fixture = (source / 'crates/markdown_editor/installed-fixture.md').read_bytes()
runs = []
for name in ['wayland', 'x11', 'missing-runtime']:
    run = root / name
    configuration = run / 'user-data/config'
    configuration.mkdir(parents=True, exist_ok=True)
    (configuration / 'settings.json').write_text(json.dumps({'auto_update':False,'disable_ai':True,'auto_install_extensions':{'html':False},'language_servers':[],'languages':{'Markdown':{'language_servers':[],'formatter':'none','prettier':{'allowed':False}}},'markdown_editor':{'default_open_mode':'editor','mode':'instant_rendering','plantuml_service':None},'markdown_preview':{'open_markdown_files_in_preview':True}}))
    (run / 'fixture.md').write_bytes(fixture)
    report = run / 'report.json'
    assert not report.exists(), f'Existing acceptance report: {report}'
    current_environment = environment.copy()
    current_environment['LATHE_MARKDOWN_INSTALLATION_CHECK'] = str(report)
    if name == 'x11':
        current_environment.update(WAYLAND_DISPLAY='', DISPLAY=':0', XDG_SESSION_TYPE='x11')
    else:
        current_environment.pop('DISPLAY', None)
        current_environment.update(WAYLAND_DISPLAY='/run/user/1000/wayland-0', XDG_SESSION_TYPE='wayland')
    if name == 'missing-runtime':
        incomplete = run / 'incomplete-runtime'
        incomplete.mkdir()
        current_environment['LATHE_MARKDOWN_CEF_RUNTIME'] = str(incomplete)
    owned = {}
    with (run / 'stdout.log').open('wb') as stdout, (run / 'stderr.log').open('wb') as stderr:
        process = subprocess.Popen([str(executable), '--user-data-dir', str(run / 'user-data'), str(run / 'fixture.md')], env=current_environment, cwd=installation / 'libexec', stdout=stdout, stderr=stderr)
        deadline = time.monotonic() + 180
        while process.poll() is None and time.monotonic() < deadline:
            for entry in Path('/proc').iterdir():
                if not entry.name.isdecimal():
                    continue
                try:
                    if (entry / 'exe').resolve() == executable:
                        command = (entry / 'cmdline').read_bytes().replace(b'\0', b' ').decode(errors='replace')
                        owned[entry.name] = {'process_id':int(entry.name),'executable':str(executable),'command_line':command}
                except (OSError, RuntimeError):
                    continue
            time.sleep(.25)
        assert process.poll() is not None, f'Installed process {process.pid} timed out; retained for diagnosis'
    assert report.exists(), f'{name} exited {process.returncode} without report'
    assert process.returncode == 0, f'{name} exited {process.returncode}'
    result = json.loads(report.read_text())
    if name == 'missing-runtime':
        assert not result['success'] and result['source_preserved'] and 'Reinstall' in result['error'], result
    else:
        assert result['success'] and result['source_preserved'] and len(result['modes']) == 4, result
        assert any('--type=' in item['command_line'] for item in owned.values()), owned
    assert (run / 'fixture.md').read_bytes() == fixture
    runs.append({'name':name,'result':result,'exit_code':process.returncode,'processes':list(owned.values()),'fixture_sha256':hashlib.sha256(fixture).hexdigest(),'display':current_environment.get('DISPLAY'),'wayland_display':current_environment['WAYLAND_DISPLAY'],'session_type':current_environment['XDG_SESSION_TYPE']})
    print(f'INSTALLED_{name.upper()}_PASSED', flush=True)
manifest = json.loads((installation / 'libexec/markdown-runtime.json').read_text())
for item in manifest['files']:
    assert hashlib.sha256((installation / 'libexec' / item['path']).read_bytes()).hexdigest() == item['sha256']
output = {'archive':str(archive),'archive_sha256':hashlib.sha256(archive.read_bytes()).hexdigest(),'executable':str(executable),'executable_sha256':hashlib.sha256(executable.read_bytes()).hexdigest(),'node_on_path':False,'proxy':'http://127.0.0.1:9','cef_override_normal':None,'ld_library_path':None,'ld_preload':None,'runtime_files_verified':len(manifest['files']),'ldd':dependencies,'bundled_dependencies':bundled_dependencies,'runs':runs}
(root / 'acceptance.json').write_text(json.dumps(output, indent=2))
print('INSTALLED_LINUX_CHECKS_PASSED', flush=True)
