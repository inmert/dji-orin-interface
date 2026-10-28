// Offline check of the thermal click mapping: frame fractions → spot-meter
// coordinates over the 5:4 thermal image centred in the 16:9 video.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const ts = require('typescript');
const exportsUnderTest = {};
vm.runInNewContext(ts.transpileModule(fs.readFileSync('src/gimbal/Thermal.tsx', 'utf8'), {
  compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2020 },
}).outputText, { exports: exportsUnderTest, require: name => name === 'react' ? { useState: v => [v, () => {}], useEffect: () => {} } : { jsx: () => null, jsxs: () => null, Fragment: null } });
const { frameToThermal } = exportsUnderTest;
const close = (got, want) => got.forEach((v, i) => assert.ok(Math.abs(v - want[i]) < 1e-9, `${got} vs ${want}`));
close(frameToThermal(0.5, 0.5), [0.5, 0.5]); // centre maps to centre
close(frameToThermal(190 / 1280, 0), [0, 0]); // left edge of the thermal image
close(frameToThermal(1090 / 1280, 1), [1, 1]); // right edge
assert.equal(frameToThermal(0.05, 0.5), null); // black bar
assert.equal(frameToThermal(0.95, 0.5), null);
console.log('PASS: thermal click mapping');
const { validBand } = exportsUnderTest;
assert.ok(validBand(27, 32, 38));
assert.ok(validBand(-40, 0, 800));
assert.ok(validBand(20, 20, 20)); // equal bounds are allowed (the camera accepted 32/32)
assert.ok(!validBand(30, 20, 40)); // out of order
assert.ok(!validBand(27.5, 32, 38)); // whole °C only
assert.ok(!validBand(-41, 0, 10) && !validBand(0, 10, 801)); // out of range
assert.ok(!validBand(NaN, 0, 10)); // empty input
const { parseBound } = exportsUnderTest;
assert.ok(Number.isNaN(parseBound('')) && Number.isNaN(parseBound('  '))); // cleared field
assert.equal(parseBound('-40'), -40);
assert.ok(!validBand(...['', '30', '40'].map(parseBound))); // not sent as 0 °C
console.log('PASS: isotherm band validation');
