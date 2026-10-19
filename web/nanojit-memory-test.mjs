import assert from 'node:assert/strict';
import { extractMemoryTrace, lowerCapturedMemory } from './nanojit-memory.js';

function fixture(write) {
  const records=[];
  function add(opcode,words=[]) {
    const bytes=new Uint8Array((words.length+1)*4),view=new DataView(bytes.buffer);
    words.forEach((word,i)=>view.setUint32(i*4,word,true));bytes[bytes.length-1]=opcode;
    const record={address:0x1000+records.length*32,opcode,bytes:[...bytes]};records.push(record);return record.address;
  }
  add(0);add(4,[256]);add(4,[257]);add(4,[258]);add(4,[0]);add(4,[1]);const argv=add(4,[2]);add(6,[3]);add(0x39,[0x30000]);
  while(records.length<20)add(12,[0]);
  const receiver=add(0x1b,[0,argv]);
  if(write){const value=add(0x1b,[4,argv]);add(0x23,[88,receiver,value]);}
  const read=add(0x1b,[88,receiver]);add(7,[read]);
  for(let i=0;i<4;i++)add(12,[receiver]);
  return {records,code:0x5000,nativePrefix:[0x55,0xc3]};
}
const get=extractMemoryTrace(fixture(false));
assert.deepEqual(get.bindings,[0x80000058]);assert.deepEqual(get.fields,[88]);assert.equal(get.store,null);assert.equal(get.argc,0);
const set=extractMemoryTrace(fixture(true));
assert.deepEqual(set.bindings,[4]);assert.equal(set.store,88);assert.equal(set.argc,1);
assert.match(set.source,/reti m21/,'post-store read must forward the staged value, not the old heap value');
for(const mutate of [
  c=>{c.records[23].bytes[0]=89;},
  c=>{c.records[23].bytes[4]=0;},
  c=>{c.records[23].opcode=0x75;},
  c=>{c.records[23].address=c.records[22].address;},
]){const c=fixture(true);mutate(c);assert.equal(extractMemoryTrace(c),null);}
const refused=lowerCapturedMemory({installCapturedMemoryEffects(){throw new Error('native memory proof rejected');}},fixture(true));
assert.equal(refused.count,0);assert.equal(refused.reason,'native memory proof rejected');
console.log('PASS receiver-field extraction, staged-store forwarding and malformed/native-proof rejection');
