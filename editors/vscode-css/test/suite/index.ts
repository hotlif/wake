import * as vscode from 'vscode'

export async function run(): Promise<void> {
  let phase = 'startup'
  try {
    await runSuite(value => { phase = value })
  } catch (error) {
    const detail = error instanceof Error ? error.stack ?? error.message : String(error)
    const message = `${phase}: ${detail}`
    console.error(`::error title=Crab CSS Extension Host::${escapeWorkflowCommand(message)}`)
    throw error
  }
}

async function runSuite(setPhase: (value: string) => void): Promise<void> {
  setPhase('extension activation')
  const extension = vscode.extensions.getExtension('crab-dev.crab-css')
  if (!extension) throw new Error('Crab CSS extension is not registered')
  await extension.activate()

  setPhase('automatic completion document setup')
  const automaticDocument = await vscode.workspace.openTextDocument({
    language: 'typescriptreact',
    content: "import { css } from '@crab-dev/css'\nconst automatic = css``\nconst ordinary = ''\n",
  })
  const automaticEditor = await vscode.window.showTextDocument(automaticDocument)
  const automaticPosition = automaticDocument.positionAt(
    automaticDocument.getText().indexOf('``') + 1,
  )
  setPhase('automatic completion provider readiness')
  await waitFor(async () => {
    const completions = await vscode.commands.executeCommand<vscode.CompletionList>(
      'vscode.executeCompletionItemProvider',
      automaticDocument.uri,
      automaticPosition,
    )
    return completions?.items.some(item => item.label === 'display') ?? false
  }, 'CSS completion provider was not ready for the automatic-suggestion document')
  setPhase('automatic property typing')
  await vscode.commands.executeCommand('workbench.action.focusActiveEditorGroup')
  automaticEditor.selection = new vscode.Selection(automaticPosition, automaticPosition)
  for (const character of 'disp') {
    await vscode.commands.executeCommand('type', { text: character })
    await new Promise(resolve => setTimeout(resolve, 20))
  }
  setPhase('automatic property suggestion acceptance')
  await waitFor(async () => {
    await vscode.commands.executeCommand('acceptSelectedSuggestion')
    return automaticDocument.getText().includes('css`display: `')
  }, 'typing a CSS property prefix did not open and accept the display completion')
  setPhase('automatic value suggestion acceptance')
  await waitFor(async () => {
    await vscode.commands.executeCommand('acceptSelectedSuggestion')
    return automaticDocument.getText().includes('css`display: block`')
  }, 'accepting a CSS property did not open and accept its top-ranked value completion')

  setPhase('ordinary TypeScript string isolation')
  const ordinaryPosition = automaticDocument.positionAt(
    automaticDocument.getText().indexOf("''") + 1,
  )
  automaticEditor.selection = new vscode.Selection(ordinaryPosition, ordinaryPosition)
  for (const character of 'disp') {
    await vscode.commands.executeCommand('type', { text: character })
    await new Promise(resolve => setTimeout(resolve, 20))
  }
  await new Promise(resolve => setTimeout(resolve, 300))
  await vscode.commands.executeCommand('acceptSelectedSuggestion')
  if (!automaticDocument.getText().includes("ordinary = 'disp'")) {
    throw new Error('CSS completion changed an ordinary TypeScript string')
  }

  setPhase('fixture completion provider')
  const uri = vscode.Uri.joinPath(vscode.workspace.workspaceFolders![0].uri, 'component.tsx')
  const document = await vscode.workspace.openTextDocument(uri)
  await vscode.window.showTextDocument(document)
  await waitFor(async () => {
    const position = document.positionAt(document.getText().indexOf('disp') + 4)
    const completions = await vscode.commands.executeCommand<vscode.CompletionList>(
      'vscode.executeCompletionItemProvider',
      uri,
      position,
    )
    return completions?.items.some(item => item.label === 'display') ?? false
  }, 'CSS completion was not returned')

  setPhase('fixture formatting')
  const edits = await vscode.commands.executeCommand<vscode.TextEdit[]>(
    'vscode.executeFormatDocumentProvider',
    uri,
    { tabSize: 2, insertSpaces: true },
  )
  if (!edits?.length) throw new Error('Crab CSS formatter returned no edits')

  setPhase('fixture diagnostics')
  await document.save()
  await waitFor(
    async () => vscode.languages.getDiagnostics(uri).some(diagnostic => diagnostic.source === 'crab-css'),
    'Crab CSS diagnostics were not published',
  )

  setPhase('language server restart')
  await vscode.commands.executeCommand('crabCss.restartLanguageServer')
}

function escapeWorkflowCommand(value: string): string {
  return value.replaceAll('%', '%25').replaceAll('\r', '%0D').replaceAll('\n', '%0A')
}

async function waitFor(check: () => Promise<boolean>, message: string): Promise<void> {
  const deadline = Date.now() + 10_000
  while (Date.now() < deadline) {
    if (await check()) return
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(message)
}
