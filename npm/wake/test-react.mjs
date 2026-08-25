import api from './test-react.cjs'

export {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  clock,
  describe,
  expect,
  it,
  mock,
  network,
  test,
} from './test.mjs'

export const {
  act,
  cleanup,
  fireEvent,
  prettyDOM,
  render,
  renderHook,
  screen,
  userEvent,
  waitFor,
  waitForElementToBeRemoved,
  within,
} = api
