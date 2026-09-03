import api from './index.cjs'

export const {
  BuildContext,
  DevServer,
  TestContext,
  WakeError,
  build,
  buildLibrary,
  buildDocs,
  bundle,
  runTests,
  generateCssToken,
  generateDocgen,
  initializeFederation,
  generateFederationLock,
  createBuildContext,
  createTestContext,
  startDevServer,
  startDocsDevServer,
  version,
} = api

export default api
