import * as editorWorkerService from 'monaco-editor/editor/browser/services/editorWorkerService.js';

// Retain the real dependency module containing Monaco's static Worker URL.
globalThis.monacoWorkerService = editorWorkerService;
