// Reproduce without network/transactions: install @ethereumjs/vm@10.1.0 under a temporary
// prefix, copy this script there, and pass the absolute gauge_rate_evidence.json path.
import { readFileSync } from 'node:fs';
import assert from 'node:assert/strict';
import { createVM } from '@ethereumjs/vm';
import { Common, Mainnet, Hardfork } from '@ethereumjs/common';
import { Account, createAddressFromString, hexToBytes, bytesToHex } from '@ethereumjs/util';
import { keccak256 } from 'ethereum-cryptography/keccak.js';
const fixture=JSON.parse(readFileSync(process.argv[2]));
assert.equal(bytesToHex(keccak256(hexToBytes(fixture.runtime))),fixture.runtime_keccak256);
const word=n=>BigInt(n).toString(16).padStart(64,'0');
const selector=s=>bytesToHex(keccak256(new TextEncoder().encode(s))).slice(2,10);
for(const c of fixture.cases){
 const vm=await createVM({common:new Common({chain:Mainnet,hardfork:Hardfork.Istanbul})});
 const target=createAddressFromString(fixture.contract.toLowerCase());
 const sender=createAddressFromString('0x0000000000000000000000000000000000000099');
 const token=createAddressFromString('0x0000000000000000000000000000000000000088');
 for(const address of [target,sender,token])await vm.stateManager.putAccount(address,new Account(0n,10n**30n));
 await vm.stateManager.putCode(target,hexToBytes(fixture.runtime));
 await vm.stateManager.putCode(token,hexToBytes(c.mock_runtime));
 const put=async(k,v)=>vm.stateManager.putStorage(target,hexToBytes('0x'+word(k)),hexToBytes('0x'+word(v)));
 const call=async(sig,args=[])=>{
  const r=await vm.evm.runCall({to:target,caller:sender,origin:sender,gasLimit:20000000n,data:hexToBytes('0x'+selector(sig)+args.map(word).join(''))});
  assert.equal(r.execResult.exceptionError,undefined,`${sig}: ${bytesToHex(r.execResult.returnValue)}`);
  return bytesToHex(r.execResult.returnValue).slice(2).match(/.{64}/g)?.map(w=>BigInt('0x'+w))??[];
 };
 // Constructor/storage setup only. Production gauge's own methods perform all rate arithmetic.
 await put(0,1); // reentrancy guard
 await put(2,1); // pool array length
 const poolBase=BigInt(bytesToHex(keccak256(hexToBytes('0x'+word(2)))));
 await put(poolBase,0x77);await put(poolBase+1n,10n**18n);
 await put(BigInt('0x7291c984ccbc3e2dc0a5be162f04ac08ce1496feb78a647b31c362fd8093f090'),1);
 assert.equal((await call('REWARD_RATE_PRECISION()'))[0],10n**18n);
 assert.equal((await call('rewardTokenAllowed(address)',[0x88]))[0],1n);
 await call('notifyRewardAmount(uint256,address,uint256,uint256)',[0,0x88,c.funding_atoms,c.duration_seconds]);
 const data=await call('rewardData(uint256,address)',[0,0x88]);
 const summary=await call('rewardStreamSummary(uint256,address)',[0,0x88]);
 assert.equal(data[1],BigInt(c.reward_rate));
 assert.equal(summary[1],BigInt(c.reward_rate));
 assert.equal(summary[2],BigInt(c.remaining_atoms));
 console.log(`${c.decimals} decimals: rate=${data[1]}, remaining=${summary[2]}; 100 tokens/day`);
}
