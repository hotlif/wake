export default class WakeDeniedWebSocket {
    constructor() {
        throw Object.assign(
            new Error('WebSocket access is denied by the Wake test network policy.'),
            { code: 'WAKE_TEST_NETWORK' }
        );
    }
}
