const fs = require('fs');
const c = fs.readFileSync('C:/Users/zhang/Desktop/SourceCode/RustProject/wake/fixtures/react-ts-app/dist/bundle.js', 'utf8');
try {
    new Function(c);
} catch(e) {
    console.log('Error message:', e.message);
    // Find position from stack or message
    const match = e.message.match(/position (\d+)/);
    if (match) {
        const idx = parseInt(match[1]);
        console.log('Error at position:', idx);
        const start = Math.max(0, idx - 100);
        const end = Math.min(c.length, idx + 100);
        const before = c.substring(start, idx);
        const after = c.substring(idx, end);
        console.log('Context before:', JSON.stringify(before.slice(-80)));
        console.log('Error char:', JSON.stringify(c[idx]));
        console.log('Context after:', JSON.stringify(after.slice(0, 80)));
    } else {
        console.log('No position in message');
    }
    console.log('Full error:', e.toString());
}
