import * as vscode from 'vscode'
import { LanguageClient, LanguageClientOptions, ServerOptions } from 'vscode-languageclient/node'

let client: LanguageClient | undefined

function serverOptions(): ServerOptions {
  const configured = vscode.workspace.getConfiguration('wakeLint').get<string>('serverPath') || 'wake-lint-language-server'
  return { command: configured, args: [], options: { cwd: vscode.workspace.workspaceFolders?.[0]?.uri.fsPath } }
}

function clientOptions(): LanguageClientOptions {
  return {
    documentSelector: [
      { scheme: 'file', language: 'javascript' },
      { scheme: 'file', language: 'javascriptreact' },
      { scheme: 'file', language: 'typescript' },
      { scheme: 'file', language: 'typescriptreact' },
    ],
    synchronize: { configurationSection: 'wakeLint' },
  }
}

async function start(): Promise<void> {
  if (!vscode.workspace.getConfiguration('wakeLint').get<boolean>('enable', true)) return
  client = new LanguageClient('wakeLint', 'Wake Lint', serverOptions(), clientOptions())
  await client.start()
}

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  context.subscriptions.push(vscode.commands.registerCommand('wakeLint.restart', async () => {
    await client?.stop()
    await start()
  }))
  context.subscriptions.push({ dispose: () => { void client?.stop() } })
  await start()
}

export async function deactivate(): Promise<void> {
  await client?.stop()
}
