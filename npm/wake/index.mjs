import api from './index.cjs'

export const {
  BuildContext,
  LintContext,
  DevServer,
  TestContext,
  WakeError,
  build,
  buildLibrary,
  buildDocs,
  bundle,
  lint,
  runTests,
  generateCssToken,
  generateDocgen,
  initializeFederation,
  generateFederationLock,
  createBuildContext,
  createLintContext,
  createTestContext,
  startDevServer,
  startDocsDevServer,
  version,
} = api

export default api
