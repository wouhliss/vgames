const expected = '/synthetic-public-argument';
const argumentPreserved = process.argv.at(-1) === expected;
const environmentPreserved = process.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD === expected;
process.stdout.write(JSON.stringify({argumentPreserved, environmentPreserved}) + '\n');
if (!argumentPreserved || !environmentPreserved) process.exitCode = 1;
