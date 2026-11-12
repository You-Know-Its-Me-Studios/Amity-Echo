const vscode = require('vscode');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { spawn } = require('child_process');

function activate(context) {
  const output = vscode.window.createOutputChannel('Amity Echo');
  context.subscriptions.push(output);
  context.subscriptions.push(vscode.commands.registerCommand('amityEcho.ask', async () => {
    const editor = vscode.window.activeTextEditor;
    if (!editor || editor.document.uri.scheme !== 'file') {
      vscode.window.showErrorMessage('Open a project file before asking Echo.');
      return;
    }
    const instruction = await vscode.window.showInputBox({
      prompt: 'What should Echo investigate or change?',
      ignoreFocusOut: true,
    });
    if (!instruction || !instruction.trim()) return;
    const diagnostics = vscode.languages.getDiagnostics(editor.document.uri).slice(0, 50).map(item => ({
      line: item.range.start.line + 1,
      severity: ['error', 'warning', 'information', 'hint'][item.severity] || 'info',
      message: item.message,
    }));
    const selected = editor.document.getText(editor.selection);
    const contextPath = path.join(os.tmpdir(), `amity-echo-editor-${process.pid}-${Date.now()}.json`);
    await fs.promises.writeFile(contextPath, JSON.stringify({
      file: editor.document.uri.fsPath,
      selection: selected.slice(0, 32768),
      diagnostics,
    }), { mode: 0o600, flag: 'wx' });
    const folder = vscode.workspace.getWorkspaceFolder(editor.document.uri);
    const cwd = folder ? folder.uri.fsPath : path.dirname(editor.document.uri.fsPath);
    output.clear();
    output.show(true);
    output.appendLine(`Echo · ${path.basename(editor.document.uri.fsPath)}`);
    const child = spawn('echocli', ['--json', '--editor-context', contextPath, instruction], {
      cwd, shell: false, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'],
    });
    let pending = '';
    child.stdout.setEncoding('utf8');
    child.stdout.on('data', chunk => {
      pending += chunk;
      for (;;) {
        const lineEnd = pending.indexOf('\n');
        if (lineEnd < 0) break;
        const line = pending.slice(0, lineEnd);
        pending = pending.slice(lineEnd + 1);
        try {
          const event = JSON.parse(line);
          if (event.type === 'assistant') output.appendLine(`\n${event.message}\n`);
          else if (event.type === 'operation') output.appendLine(`↳ ${event.detail}`);
          else if (event.type === 'tool_request') output.appendLine(`↳ ${event.tool}: ${event.detail}`);
          else if (event.type === 'warning' || event.type === 'error') output.appendLine(`${event.type}: ${event.message}`);
          else if (event.type === 'result') {
            output.appendLine(`\nStatus: ${event.status}`);
            if (event.exit_code === 2 || event.exit_code === 3) {
              output.appendLine('Open echocli in this project to review the pending decision.');
            }
          }
        } catch (_) { output.appendLine(line); }
      }
    });
    child.stderr.setEncoding('utf8');
    child.stderr.on('data', chunk => output.append(chunk));
    child.on('error', error => output.appendLine(`Could not start echocli: ${error.message}`));
    child.on('close', async code => {
      await fs.promises.unlink(contextPath).catch(() => {});
      if (pending.trim()) output.appendLine(pending.trim());
      if (code && code !== 2 && code !== 3) output.appendLine(`Echo exited with code ${code}.`);
    });
  }));
}

function deactivate() {}

module.exports = { activate, deactivate };
