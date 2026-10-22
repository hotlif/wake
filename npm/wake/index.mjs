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
  testDocs,
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
