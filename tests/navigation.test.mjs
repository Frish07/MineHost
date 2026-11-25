import test from 'node:test';
import assert from 'node:assert/strict';
import {addonKind,serverPages} from '../src/navigation.ts';
test('server capabilities expose only the matching addon section',()=>{
 for(const core of ['paper','purpur']){assert.equal(addonKind(core),'plugins');assert.ok(serverPages(core).includes('plugins'));assert.ok(!serverPages(core).includes('mods'));assert.ok(!serverPages(core).includes('modpacks'));}
 for(const core of ['fabric','forge','neoforge']){assert.equal(addonKind(core),'mods');assert.ok(serverPages(core).includes('mods'));assert.ok(!serverPages(core).includes('plugins'));assert.ok(serverPages(core).includes('modpacks'));}
 assert.equal(addonKind('vanilla'),null);assert.ok(!serverPages('vanilla').some(p=>['mods','plugins','modpacks'].includes(p)));
});
