"""Real native-engine + ConPTY terminal acceptance. Writes only Echo artifacts.

Requires pywinpty/pyte in the test interpreter, built Echo/Cadenza executables,
and Cadenza's existing absolute local development configuration (read-only).
Credentials are random process-local environment values, never saved.
"""
import argparse
import json
import os
from pathlib import Path
import secrets
import subprocess
import sys
import threading
import time

class Terminal:
    def __init__(self, executable, cwd, env):
        from winpty import PtyProcess
        import pyte
        self.process=PtyProcess.spawn([str(executable)],cwd=str(cwd),env=env,dimensions=(44,160))
        self.screen=pyte.Screen(160,44);self.parser=pyte.Stream(self.screen);self.lock=threading.Lock();self.transcript=[]
        self.reader=threading.Thread(target=self.read,daemon=True);self.reader.start()
    def read(self):
        try:
            while self.process.isalive():
                data=self.process.read(65536)
                with self.lock:self.transcript.append(data);self.parser.feed(data)
        except (EOFError,OSError):pass
    def display(self):
        with self.lock:return '\n'.join(self.screen.display)
    def wait(self, text, timeout=25):
        end=time.monotonic()+timeout
        while time.monotonic()<end:
            if text in self.display():return self.display()
            time.sleep(.05)
        raise AssertionError(f'Terminal did not show {text!r}:\n{self.display()}')
    def type(self,text):self.process.write(text)
    def stop(self):
        if self.process.isalive():self.type('/exit\r')
        end=time.monotonic()+5
        while self.process.isalive() and time.monotonic()<end:time.sleep(.05)
        if self.process.isalive():self.process.terminate(force=True)

def main():
    p=argparse.ArgumentParser();p.add_argument('--cadenza-root',required=True);p.add_argument('--cadenza-config',required=True);p.add_argument('--echo',required=True);p.add_argument('--output',required=True)
    a=p.parse_args();root=Path(__file__).resolve().parents[1];cadenza=Path(a.cadenza_root).resolve();output=Path(a.output).resolve();output.mkdir(parents=True,exist_ok=False)
    executable=Path(a.echo).resolve();home=output/'home';home.mkdir();other=output/'other-workspace';other.mkdir();work=output/'workspace';work.mkdir()
    sys.path.insert(0,str(cadenza/'tools'));from client import Client
    configuration=json.loads(Path(a.cadenza_config).read_text(encoding='utf-8'));token=secrets.token_hex(32)
    env=dict(os.environ);env['CADENZA_API_TOKEN']=token;env['AMITY_ECHO_HOME']=str(home);env['ECHO_CADENZA_REPORT']=str(output/'protocol.json');env['PYTHONDONTWRITEBYTECODE']='1'
    env.pop('AMITY_CADENZA_HOME',None)
    env[configuration['api_token_env']]=token
    env['ECHO_AMITY_REPORT']=str(output/'amity.json')
    cuda=Path(env['CUDA_PATH']);env['PATH']=str(cuda/'bin')+os.pathsep+env['PATH']
    settings={'endpoint':'ws://127.0.0.1:7941/cadenza','credential_ref':'env:CADENZA_API_TOKEN','default_model':None,'connect_timeout_seconds':5,'request_timeout_seconds':120,'max_tokens':8}
    # Exercise actual migration on an isolated old-style application config.
    actual_home=Path(os.environ['LOCALAPPDATA'])/'Amity'/'Echo'
    owner_config=json.loads((actual_home/'config.json').read_text(encoding='utf-8'))
    initial_config={'projects':[{'id':'legacy-project','name':'legacy-project','path':str(work),'host_config':None,'models':[],'selected_model':None}],
        'host_executable':owner_config['host_executable'],'cadenza':settings,'session_dir':str(home/'sessions')}
    initial_bytes=json.dumps(initial_config).encode('utf-8');(home/'config.json').write_bytes(initial_bytes)
    service_log=(output/'service.log').open('w',encoding='utf-8');service=subprocess.Popen([str(cadenza/'target/release/cadenza.exe'),'serve','--config',str(Path(a.cadenza_config).resolve())],cwd=output,env=env,stdout=service_log,stderr=subprocess.STDOUT)
    terminal=None
    try:
        end=time.monotonic()+90
        while True:
            try:
                with Client(settings['endpoint'],token) as client:
                    client.send({'type':'discover'});models=client.receive()['models'];client.send({'type':'status'});assert client.receive()['worker_alive'];break
            except (OSError,EOFError,ValueError):
                if service.poll() is not None or time.monotonic()>end:raise AssertionError('Cadenza did not start; inspect service.log')
                time.sleep(.2)
        # Real Rust protocol acceptance: discovery, generation, cumulative events,
        # cancellation, usage, reconnect, bad auth and missing model rejection.
        with (output/'protocol-tests.txt').open('w',encoding='utf-8') as log:
            subprocess.run(['cargo','test','--locked','cadenza::tests::real_cadenza_acceptance','--','--ignored','--nocapture'],cwd=root,env=env,stdout=log,stderr=subprocess.STDOUT,check=True,timeout=180)
        terminal=Terminal(executable,work,env);terminal.wait('1 model available. Use /model to select.')
        assert (home/'config.pre-cadenza.json').read_bytes()==initial_bytes
        assert all('models' not in p and 'selected_model' not in p for p in json.loads((home/'config.json').read_text(encoding='utf-8'))['projects'])
        terminal.type('/model\r');screen=terminal.wait('Select Model and Effort');assert 'Text generation' in screen
        (output/'picker.txt').write_text(screen,encoding='utf-8');terminal.type('\r');terminal.wait('Model Switched to ')
        saved=json.loads((home/'config.json').read_text(encoding='utf-8'));assert saved['cadenza']['default_model']['root']==models[0]['root']
        terminal.type('/model\r');terminal.wait('(current)');terminal.type('\x1b');time.sleep(.3)
        assert json.loads((home/'config.json').read_text(encoding='utf-8'))['cadenza']==saved['cadenza']
        terminal.type('The meaning of life is\r');screen=terminal.wait('Response complete',timeout=90)
        (output/'generation-screen.txt').write_text(screen,encoding='utf-8')
        (work/'safe.txt').write_text('public acceptance fixture',encoding='utf-8')
        terminal.type('/init\r');terminal.wait('Amity is ready',timeout=60)
        # The installed host's workspace-init baseline places candidate workspaces
        # inside project_root, which agent-tool correctly rejects as overlapping.
        # Give this isolated fixture a non-overlapping candidate base; retain the
        # exact ACL/authority/actor settings and leave the generated host file intact.
        active=json.loads((home/'config.json').read_text(encoding='utf-8'))
        project=next(p for p in active['projects'] if p['id']==active['active_project'])
        binding=json.loads(Path(project['host_config']).read_text(encoding='utf-8'))
        candidate_base=home/'candidate-workspaces';candidate_base.mkdir();binding['workspace_base']=str(candidate_base)
        binding_path=home/'governance-acceptance.json';binding_path.write_text(json.dumps(binding),encoding='utf-8')
        project['host_config']=str(binding_path);(home/'config.json').write_text(json.dumps(active),encoding='utf-8')
        with (output/'amity-tests.txt').open('w',encoding='utf-8') as log:
            subprocess.run(['cargo','test','--locked','host::cadenza_tests::real_amity_permissions_remain_enforced','--','--ignored','--nocapture'],cwd=root,env=env,stdout=log,stderr=subprocess.STDOUT,check=True,timeout=60)
        callback=subprocess.run([str(executable),'__cadenza_turn',models[0]['root']],input=json.dumps({'instruction':'The meaning of life is'}),capture_output=True,text=True,env=env,timeout=60,check=True)
        assert json.loads(callback.stdout)['type']=='final_proposal'
        (output/'native-host-callback.json').write_text(callback.stdout,encoding='utf-8')
        terminal.stop();terminal=None
        # New launch, different working directory, same global default, real text.
        result=subprocess.run([str(executable),'--json','The meaning of life is'],cwd=other,env=env,capture_output=True,text=True,check=True,timeout=90)
        events=[json.loads(line) for line in result.stdout.splitlines()];assert any(e['type']=='model_text' for e in events);assert any(e['type']=='assistant' for e in events);assert events[-1]['exit_code']==0;assert events[-1]['usage']['output_tokens']>0
        oracle=json.loads((output/'protocol.json').read_text(encoding='utf-8'))['generated_text']
        assert next(e['message'] for e in events if e['type']=='assistant')==oracle
        (output/'cross-directory.json').write_text(json.dumps(events,indent=2),encoding='utf-8')
        for session_path in (home/'sessions').glob('*.json'):
            session=json.loads(session_path.read_text(encoding='utf-8'));assert session['cadenza_model']['root']==models[0]['root']
        terminal=Terminal(executable,other,env);terminal.wait('Ready');terminal.type('/model\r');screen=terminal.wait('(current)');(output/'restart-picker.txt').write_text(screen,encoding='utf-8');terminal.type('\x1b');time.sleep(.2);terminal.stop();terminal=None
        desktop=Path.home()/'Desktop'
        no_credential_env=dict(env);no_credential_env.pop(configuration['api_token_env'],None)
        terminal=Terminal(executable,desktop,no_credential_env);terminal.wait('Model authentication is unavailable.');terminal.stop();terminal=None
        # Unavailable service: UI stays usable, picker errors, selection is unchanged.
        service.terminate();service.wait(timeout=15)
        terminal=Terminal(executable,desktop,no_credential_env);terminal.wait('Models unavailable. Try again later.');terminal.type('/model\r');screen=terminal.wait('Models unavailable. Try again later.');(output/'offline-picker.txt').write_text(screen,encoding='utf-8')
        assert json.loads((home/'config.json').read_text(encoding='utf-8'))['cadenza']['default_model']['root']==models[0]['root']
        terminal.stop();terminal=None
        service=subprocess.Popen([str(cadenza/'target/release/cadenza.exe'),'serve','--config',str(Path(a.cadenza_config).resolve())],cwd=output,env=env,stdout=service_log,stderr=subprocess.STDOUT)
        end=time.monotonic()+90
        while True:
            try:
                with Client(settings['endpoint'],token):break
            except (OSError,EOFError,ValueError):
                if service.poll() is not None or time.monotonic()>end:raise AssertionError('Cadenza did not restart')
                time.sleep(.2)
        reconnect=subprocess.run([str(executable),'--json','Continue'],cwd=other,env=env,capture_output=True,text=True,check=True,timeout=90)
        (output/'reconnected.jsonl').write_text(reconnect.stdout,encoding='utf-8')
        assert json.loads(reconnect.stdout.splitlines()[-1])['exit_code']==0
        # Exercise Ctrl+C through Echo's actual terminal while real tokens stream.
        cancel_config=json.loads((home/'config.json').read_text(encoding='utf-8'))
        protocol=json.loads((output/'protocol.json').read_text(encoding='utf-8'))
        cancel_config['cadenza']['max_tokens']=min(256,models[0]['max_context']-protocol['prompt_tokens'])
        (home/'config.json').write_text(json.dumps(cancel_config),encoding='utf-8');cancel_work=output/'cancel-workspace';cancel_work.mkdir()
        terminal=Terminal(executable,cancel_work,env);terminal.wait('Ready');terminal.type('The meaning of life is\r');terminal.wait('that it',timeout=15);terminal.type('\x03')
        screen=terminal.wait('Cadenza generation cancelled',timeout=15);(output/'cancel-screen.txt').write_text(screen,encoding='utf-8');terminal.stop();terminal=None
        cancelled=[json.loads(p.read_text(encoding='utf-8')) for p in (home/'sessions').glob('*.json') if 'cancel-workspace' in json.loads(p.read_text(encoding='utf-8')).get('working_directory','')]
        assert len(cancelled)==1 and cancelled[0]['status']=='interrupted' and cancelled[0]['usage_output_tokens']>0
        amity=json.loads((output/'amity.json').read_text(encoding='utf-8'))
        report={'discovery':True,'interactive_picker':True,'enter_selection':True,'escape_preserves_selection':True,'global_selection_persists':True,'cross_directory_generation':True,'actual_engine_output':True,'restart_picker_highlights_current':True,'legacy_config_backup':True,'amity_reads_and_permission_rules':True,'native_host_callback':True,'desktop_missing_credential_reported':True,'desktop_offline_without_credential_reported':True,'offline_picker_preserves_selection':True,'service_restart_recovery':True,'interactive_ctrl_c_cancellation':True,'amity_task_creation_passed':amity['task_creation_passed'],'amity_task_creation_error':amity['task_creation_error'],'model_root':models[0]['root'],'model_identity':models[0]['identity'],'echo_executable':str(executable)}
        (output/'result.json').write_text(json.dumps(report,indent=2),encoding='utf-8');print(json.dumps(report,indent=2))
    finally:
        if terminal:terminal.stop()
        service.terminate()
        try:service.wait(timeout=15)
        except subprocess.TimeoutExpired:service.kill();service.wait()
        service_log.close()

if __name__=='__main__':main()
