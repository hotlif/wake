export default {
  "mode": "production",
  "entry": "C:\\Users\\zhang\\Desktop\\SourceCode\\RustProject\\wake\\fixtures\\2k-modules\\input\\entry.js",
  "output": {
    "path": "C:\\Users\\zhang\\Desktop\\SourceCode\\RustProject\\wake\\fixtures\\2k-modules\\dist-webpack",
    "filename": "bundle.js",
    "clean": true
  },
  "optimization": {
    "splitChunks": false,
    "minimize": true,
    "sideEffects": true
  },
  "target": "node",
  "devtool": false,
  "stats": "errors-warnings"
};
