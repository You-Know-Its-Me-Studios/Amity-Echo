"""Real Echo/Cadenza development-connection and terminal acceptance.

The service must be running with Echo's vault credential, or the supplied
Cadenza executable/configuration must be available for Echo to start it.
No credential is passed to this script or written to its report.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess

from accept_cadenza import Terminal


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--echo', required=True)
    parser.add_argument('--cadenza-root', required=True)
    parser.add_argument('--protocol-report', required=True)
    parser.add_argument('--output', required=True)
    args = parser.parse_args()
    echo = Path(args.echo).resolve()
    root = Path(args.cadenza_root).resolve()
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    home = output / 'home'
    home.mkdir()
    first = output / 'first-project'
    second = output / 'second-project'
    cancellation_project = output / 'cancellation-project'
    first.mkdir()
    second.mkdir()
    cancellation_project.mkdir()
    source = json.loads(Path(args.protocol_report).read_text(encoding='utf-8'))
    executable = root / 'target' / 'release' / 'cadenza.exe'
    service_config = root / 'config' / 'local-lyra.json'
    config = {'cadenza': {
        'endpoint': 'ws://127.0.0.1:7941/cadenza',
        'credential_ref': 'windows:Amity/Echo/Cadenza/development',
        'development_executable': str(executable),
        'development_config': str(service_config),
        'max_tokens': 8,
    }}
    (home / 'config.json').write_text(json.dumps(config), encoding='utf-8')
    env = dict(os.environ)
    env.pop('CADENZA_API_TOKEN', None)
    env.pop('AMITY_CADENZA_HOME', None)
    env['AMITY_ECHO_HOME'] = str(home)
    terminal = None
    try:
        terminal = Terminal(echo, first, env)
        terminal.wait('Cadenza connected', timeout=120)
        terminal.type('/model\r')
        picker = terminal.wait('Cadenza models')
        assert source['model']['identity']['id'] in picker
        (output / 'model-picker.txt').write_text(picker, encoding='utf-8')
        terminal.type('\r')
        terminal.wait('Saved as the global default')
        saved = json.loads((home / 'config.json').read_text(encoding='utf-8'))
        assert saved['cadenza']['default_model']['root'] == source['model']['root']
        terminal.type('The meaning of life is\r')
        terminal.wait('Response complete', timeout=90)
        terminal.stop()
        terminal = None
        sessions = [json.loads(path.read_text(encoding='utf-8')) for path in (home / 'sessions').glob('*.json')]
        assert len(sessions) == 1
        assert sessions[0]['cadenza_model']['root'] == source['model']['root']
        assert any(message['role'] == 'assistant' and message['content'] == source['generated_text']
                   for message in sessions[0]['messages'])
        direct = subprocess.run([str(echo), '--json', 'The meaning of life is'], cwd=second,
                                env=env, capture_output=True, text=True, timeout=90, check=True)
        events = [json.loads(line) for line in direct.stdout.splitlines()]
        assert any(event['type'] == 'model_text' for event in events)
        assert any(event['type'] == 'assistant' and event['message'] == source['generated_text']
                   for event in events)
        assert events[-1]['exit_code'] == 0 and events[-1]['usage']['output_tokens'] == 8
        sessions = [json.loads(path.read_text(encoding='utf-8')) for path in (home / 'sessions').glob('*.json')]
        assert len(sessions) == 2
        assert all(session['cadenza_model']['root'] == source['model']['root'] for session in sessions)
        terminal = Terminal(echo, second, env)
        terminal.wait('Cadenza ready', timeout=30)
        terminal.type('/model\r')
        terminal.wait('[current]')
        terminal.type('\x1b')
        terminal.stop()
        terminal = None
        config = json.loads((home / 'config.json').read_text(encoding='utf-8'))
        config['cadenza']['max_tokens'] = 100
        (home / 'config.json').write_text(json.dumps(config), encoding='utf-8')
        terminal = Terminal(echo, cancellation_project, env)
        terminal.wait('Cadenza ready', timeout=30)
        terminal.type('The meaning of life is\r')
        terminal.wait('that it', timeout=20)
        terminal.type('\x03')
        terminal.wait('Cadenza generation cancelled', timeout=20)
        terminal.stop()
        terminal = None
        interrupted = [json.loads(path.read_text(encoding='utf-8')) for path in (home / 'sessions').glob('*.json')
                       if 'cancellation-project' in path.read_text(encoding='utf-8')]
        assert len(interrupted) == 1
        assert interrupted[0]['status'] == 'interrupted'
        assert interrupted[0]['usage_output_tokens'] > 0
        report = {'interactive_picker': True, 'model_identity_from_service': True,
                  'global_selection_persisted': True, 'real_generation': True,
                  'cumulative_output_exact': True, 'headless_stream': True,
                  'cross_directory_session': True, 'no_manual_token': True,
                  'interactive_cancellation': True}
        (output / 'result.json').write_text(json.dumps(report, indent=2), encoding='utf-8')
        print(json.dumps(report))
    finally:
        if terminal:
            terminal.stop()


if __name__ == '__main__':
    main()
