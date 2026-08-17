import { existsSync } from 'node:fs'
import { arch, platform } from 'node:process'
import * as vscode from 'vscode'
import {
  Executable,
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  TransportKind,
} from 'vscode-languageclient/node'

const documentSelector = [
  { language: 'javascript', scheme: 'file' },
  { language: 'javascriptreact', scheme: 'file' },
  { language: 'typescript', scheme: 'file' },
  { language: 'typescriptreact', scheme: 'file' },
  { language: 'javascript', scheme: 'untitled' },
  { language: 'javascriptreact', scheme: 'untitled' },
  { language: 'typescript', scheme: 'untitled' },
  { language: 'typescriptreact', scheme: 'untitled' },
]

let client: LanguageClient | undefined
let output: vscode.LogOutputChannel
let context: vscode.ExtensionContext

export async function activate(extensionContext: vscode.ExtensionContext): Promise<void> {
  context = extensionContext
  output = vscode.window.createOutputChannel('Crab CSS', { log: true })
  context.subscriptions.push(
    output,
    vscode.commands.registerCommand('crabCss.restartLanguageServer', restartLanguageServer),
    vscode.commands.registerCommand('crabCss.showOutput', () => output.show(true)),
    vscode.workspace.onDidChangeConfiguration(event => {
      if (event.affectsConfiguration('crabCss')) {
        void configurationChanged()
      }
    }),
  )
  await startLanguageServer()
}

export async function deactivate(): Promise<void> {
  await stopLanguageServer()
}

async function configurationChanged(): Promise<void> {
  if (!configuration().get<boolean>('enable', true)) {
    await stopLanguageServer()
    return
  }
  await startLanguageServer()
}

async function restartLanguageServer(): Promise<void> {
  await stopLanguageServer()
  await startLanguageServer()
}

async function startLanguageServer(): Promise<void> {
  if (client || !configuration().get<boolean>('enable', true)) return

  const executable = serverExecutable(context)
  if (!existsSync(executable.command)) {
    const message = `Crab CSS language server is missing for ${platform}-${arch}: ${executable.command}`
    output.appendLine(message)
    void vscode.window.showErrorMessage(message)
    return
  }

  const watcher = vscode.workspace.createFileSystemWatcher('**/*.{js,jsx,ts,tsx,mjs,cjs,mts,cts}')
  context.subscriptions.push(watcher)
  const serverOptions: ServerOptions = {
    run: executable,
    debug: executable,
  }
  const clientOptions: LanguageClientOptions = {
    documentSelector,
    outputChannel: output,
    initializationOptions: settings(),
    synchronize: {
      configurationSection: 'crabCss',
      fileEvents: watcher,
    },
  }
  client = new LanguageClient(
    'crabCss',
    'Crab CSS Language Server',
    serverOptions,
    clientOptions,
  )
  await client.start()
}

async function stopLanguageServer(): Promise<void> {
  const current = client
  client = undefined
  if (current) await current.stop()
}

function serverExecutable(extensionContext: vscode.ExtensionContext): Executable {
  const executable = platform === 'win32'
    ? 'wake-css-language-server.exe'
    : 'wake-css-language-server'
  return {
    command: extensionContext.asAbsolutePath(`server/${executable}`),
    args: ['--stdio'],
    transport: TransportKind.stdio,
    options: { env: { ...process.env, RUST_BACKTRACE: '1' } },
  }
}

function configuration(): vscode.WorkspaceConfiguration {
  return vscode.workspace.getConfiguration('crabCss')
}

function settings(): object {
  const config = configuration()
  return {
    enable: config.get<boolean>('enable', true),
    validation: { mode: config.get<string>('validation.mode', 'onType') },
    format: { enable: config.get<boolean>('format.enable', true) },
    trace: { server: config.get<string>('trace.server', 'off') },
  }
}
