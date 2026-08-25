use std::fs;
use std::path::Path;

use wake_test::{TestOptions, run_tests};

fn repository_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("wake_test is located under <repository>/crates")
}

#[test]
fn react_19_lifecycle_suspense_transition_ssr_and_hydration_conformance() {
    let fixture = tempfile::Builder::new()
        .prefix("wake-react19-conformance-")
        .tempdir_in(repository_root().join("target"))
        .unwrap();
    fs::write(fixture.path().join("package.json"), "{}").unwrap();
    fs::write(
        fixture.path().join("react19.test.tsx"),
        r#"
            import React, {
              Suspense,
              createContext,
              lazy,
              use,
              useActionState,
              useEffect,
              useId,
              useLayoutEffect,
              useContext,
              useOptimistic,
              useState,
              useTransition,
            } from 'react'
            import {createPortal} from 'react-dom'
            import {createRoot, hydrateRoot} from 'react-dom/client'
            import {renderToReadableStream, renderToString} from 'react-dom/server'
            import {prerender} from 'react-dom/static'
            import {
              act,
              cleanup,
              expect,
              render,
              renderHook,
              screen,
              test,
              userEvent,
            } from '@crab-dev/wake/test/react'

            test('StrictMode repeats render, effect, layout effect, and ref setup with cleanup', async () => {
              expect(globalThis.Deno).toBe(undefined)
              const counts = {render: 0, effect: 0, effectCleanup: 0, layout: 0, layoutCleanup: 0, ref: 0, refCleanup: 0}
              const portal = document.createElement('aside')
              document.body.appendChild(portal)

              function Lifecycle() {
                counts.render++
                useLayoutEffect(() => { counts.layout++; return () => { counts.layoutCleanup++ } }, [])
                useEffect(() => { counts.effect++; return () => { counts.effectCleanup++ } }, [])
                return createPortal(
                  <button ref={value => { if (value) counts.ref++; else counts.refCleanup++ }}>Portal ready</button>,
                  portal,
                )
              }

              await render(<Lifecycle />, {strict: true})
              expect(screen.getByRole('button', {name: 'Portal ready'})).toBeInTheDocument()
              expect(counts.render).toBeGreaterThanOrEqual(2)
              expect(counts.layout).toBeGreaterThanOrEqual(2)
              expect(counts.effect).toBeGreaterThanOrEqual(2)
              expect(counts.ref).toBeGreaterThanOrEqual(2)
              expect(counts.layoutCleanup).toBeGreaterThanOrEqual(1)
              expect(counts.effectCleanup).toBeGreaterThanOrEqual(1)
              expect(counts.refCleanup).toBeGreaterThanOrEqual(1)

              await cleanup()
              expect(counts.layoutCleanup).toBeGreaterThanOrEqual(2)
              expect(counts.effectCleanup).toBeGreaterThanOrEqual(2)
              expect(counts.refCleanup).toBeGreaterThanOrEqual(2)
            })

            test('Suspense, lazy, use(Promise), transitions, and user input settle through act', async () => {
              let releaseLazy
              const LazyValue = lazy(() => new Promise(resolve => {
                releaseLazy = () => resolve({default: () => <output>Lazy ready</output>})
              }))
              const usedValue = Promise.resolve('Promise ready')

              function UsedValue() {
                return <output>{use(usedValue)}</output>
              }
              function Transition() {
                const [value, setValue] = useState('Before transition')
                const [pending, startTransition] = useTransition()
                return <button onClick={() => startTransition(() => setValue('After transition'))}>
                  {pending ? 'Pending' : value}
                </button>
              }

              await render(<>
                <Suspense fallback={<output>Loading lazy</output>}><LazyValue /></Suspense>
                <Suspense fallback={<output>Loading promise</output>}><UsedValue /></Suspense>
                <Transition />
              </>)
              expect(screen.getByText('Loading lazy')).toBeInTheDocument()
              expect(await screen.findByText('Promise ready')).toBeInTheDocument()
              await act(async () => { releaseLazy(); await Promise.resolve() })
              expect(await screen.findByText('Lazy ready')).toBeInTheDocument()
              await userEvent.setup().click(screen.getByRole('button', {name: 'Before transition'}))
              expect(await screen.findByRole('button', {name: 'After transition'})).toBeInTheDocument()
            })

            test('Context propagates through render and renderHook wrappers', async () => {
              const Theme = createContext('default')
              function Wrapper({children}) {
                return <Theme.Provider value="wake">{children}</Theme.Provider>
              }
              function Consumer() {
                return <output>{useContext(Theme)}</output>
              }

              await render(<Consumer />, {wrapper: Wrapper})
              expect(screen.getByText('wake')).toBeInTheDocument()
              const hook = await renderHook(() => useContext(Theme), {wrapper: Wrapper})
              expect(hook.result.current).toBe('wake')
              await hook.unmount()
            })

            test('renderToString output hydrates in place and preserves pre-hydration input state', async () => {
              function HydratedForm() {
                const id = useId()
                return <label htmlFor={id}>Name<input id={id} defaultValue="server" /></label>
              }

              const markup = renderToString(<HydratedForm />)
              expect(markup).toContain('server')
              const container = document.createElement('main')
              container.innerHTML = markup
              document.body.appendChild(container)
              const serverInput = container.querySelector('input')
              serverInput.value = 'typed before hydration'
              let root
              await act(async () => {
                root = hydrateRoot(container, <HydratedForm />)
                await Promise.resolve()
              })
              const hydratedInput = container.querySelector('input')
              expect(hydratedInput).toBe(serverInput)
              expect(hydratedInput.value).toBe('typed before hydration')
              expect(container.querySelector('label').htmlFor).toBe(hydratedInput.id)
              await act(async () => root.unmount())
              expect(container.childNodes.length).toBe(0)
            })

            test('server and static Web streams preserve React markup', async () => {
              const streamed = await renderToReadableStream(<main><h1>Streamed Wake</h1></main>)
              await streamed.allReady
              const streamedHTML = await new Response(streamed).text()
              expect(streamedHTML).toContain('<h1>Streamed Wake</h1>')

              const rendered = await prerender(<article><p>Static Wake</p></article>)
              const staticHTML = await new Response(rendered.prelude).text()
              expect(staticHTML).toContain('<p>Static Wake</p>')
            })

            test('error boundaries and React 19 root callbacks retain component stacks', async () => {
              const componentStacks = []
              const rootCallbacks = []
              class Boundary extends React.Component {
                constructor(props) { super(props); this.state = {failed: false} }
                static getDerivedStateFromError() { return {failed: true} }
                componentDidCatch(_error, info) { componentStacks.push(info.componentStack) }
                render() { return this.state.failed ? <output>Recovered boundary</output> : this.props.children }
              }
              function Broken() { throw new Error('expected render failure') }

              await render(<Boundary><Broken /></Boundary>, {
                onCaughtError(error, info) { rootCallbacks.push([error.message, info.componentStack]) },
              })
              expect(screen.getByText('Recovered boundary')).toBeInTheDocument()
              expect(componentStacks.length).toBe(1)
              expect(componentStacks[0]).toContain('Broken')
              expect(rootCallbacks.length).toBe(1)
              expect(rootCallbacks[0][0]).toBe('expected render failure')
              expect(rootCallbacks[0][1]).toContain('Broken')

              await cleanup()
              const uncaught = []
              let uncaughtRenderError
              try {
                await render(<Broken />, {
                  onUncaughtError(error, info) { uncaught.push([error.message, info.componentStack]) },
                })
              } catch (error) {
                uncaughtRenderError = error
              }
              expect(uncaughtRenderError.message).toBe('expected render failure')
              // React 19.2's act() owns thrown render errors and therefore does not also call the
              // root callback. Wake's render helper preserves that official behavior.
              expect(uncaught.length).toBe(0)

              const rawContainer = document.createElement('div')
              document.body.appendChild(rawContainer)
              const rawUncaught = []
              const previousActEnvironment = globalThis.IS_REACT_ACT_ENVIRONMENT
              globalThis.IS_REACT_ACT_ENVIRONMENT = false
              try {
                const rawRoot = createRoot(rawContainer, {
                  onUncaughtError(error, info) { rawUncaught.push([error.message, info.componentStack]) },
                })
                rawRoot.render(<Broken />)
                await new Promise(resolve => setTimeout(resolve, 20))
                expect(rawUncaught.length).toBe(1)
                expect(rawUncaught[0][0]).toBe('expected render failure')
                expect(rawUncaught[0][1]).toContain('Broken')
                rawRoot.unmount()
              } finally {
                globalThis.IS_REACT_ACT_ENVIRONMENT = previousActEnvironment
                rawContainer.remove()
              }
            })

            test('hydration mismatch recovery calls onRecoverableError and commits client content', async () => {
              const container = document.createElement('section')
              container.innerHTML = '<p>server text</p>'
              document.body.appendChild(container)
              const recoveries = []
              await render(<p>client text</p>, {
                container,
                hydrate: true,
                onRecoverableError(error, info) { recoveries.push([error.message, info.componentStack]) },
              })
              expect(container.textContent).toBe('client text')
              expect(recoveries.length).toBeGreaterThanOrEqual(1)
              expect(recoveries[0][0]).toContain('Hydration failed')
              expect(recoveries[0][1]).toContain('p')
            })

            test('form Actions, useActionState, and useOptimistic settle submitted FormData', async () => {
              let submitEvents = 0
              let actionCalls = 0
              let submittedName = null
              const formRenders = []
              function ProfileForm() {
                const [saved, saveAction, pending] = useActionState(
                  async (_previous, formData) => {
                    await Promise.resolve()
                    return String(formData.get('name'))
                  },
                  'not saved',
                )
                const [optimistic, setOptimistic] = useOptimistic(saved)
                formRenders.push([saved, pending, optimistic])
                async function action(formData) {
                  actionCalls++
                  submittedName = formData.get('name')
                  setOptimistic('saving')
                  return saveAction(formData)
                }
                return <form aria-label="Profile" action={action}>
                  <input aria-label="Name" name="name" />
                  <button type="submit">Save</button>
                  <output>{pending ? 'pending' : optimistic}</output>
                </form>
              }

              await render(<ProfileForm />)
              screen.getByRole('form', {name: 'Profile'}).addEventListener('submit', () => submitEvents++)
              const user = userEvent.setup()
              const nameInput = screen.getByRole('textbox', {name: 'Name'})
              await user.type(nameInput, 'Ada')
              expect(nameInput.value).toBe('Ada')
              await user.click(screen.getByRole('button', {name: 'Save'}))
              expect(submitEvents).toBe(1)
              expect(actionCalls).toBe(1)
              expect(submittedName).toBe('Ada')
              expect(formRenders).toEqual([['not saved', false, 'not saved'], ['not saved', true, 'saving'], ['Ada', false, 'Ada']])
              expect(document.querySelector('output').textContent).toBe('Ada')
              expect(await screen.findByText('Ada')).toBeInTheDocument()
            })
        "#,
    )
    .unwrap();

    let result = run_tests(TestOptions {
        root: Some(fixture.path().to_path_buf()),
        patterns: vec!["react19.test.tsx".to_string()],
        environment: Some("dom".to_string()),
        serial: true,
        ..TestOptions::default()
    })
    .unwrap();
    assert!(result.success, "{result:#?}");
    assert_eq!(result.counts.tests.passed, 8, "{result:#?}");
    assert_eq!(result.environment.react.as_deref(), Some("19.2.8"));
    assert_eq!(result.environment.react_dom.as_deref(), Some("19.2.8"));
}
