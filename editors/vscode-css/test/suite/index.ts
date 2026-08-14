import * as vscode from 'vscode'

export async function run(): Promise<void> {
  const extension = vscode.extensions.getExtension('crab-dev.crab-css')
  if (!extension) throw new Error('Crab CSS extension is not registered')
  await extension.activate()

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

  const edits = await vscode.commands.executeCommand<vscode.TextEdit[]>(
    'vscode.executeFormatDocumentProvider',
    uri,
    { tabSize: 2, insertSpaces: true },
  )
  if (!edits?.length) throw new Error('Crab CSS formatter returned no edits')

  await document.save()
  await waitFor(
    async () => vscode.languages.getDiagnostics(uri).some(diagnostic => diagnostic.source === 'crab-css'),
    'Crab CSS diagnostics were not published',
  )

  await vscode.commands.executeCommand('crabCss.restartLanguageServer')
}

async function waitFor(check: () => Promise<boolean>, message: string): Promise<void> {
  const deadline = Date.now() + 10_000
  while (Date.now() < deadline) {
    if (await check()) return
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(message)
}
