// Install @ethereumjs/vm@10.1.0 in a temporary prefix, copy this file there, and pass
// hartii_runtime_evidence.json as argv[2]. Runs offline with no transactions or signer secrets.
import {readFileSync} from 'node:fs';
import assert from 'node:assert/strict';
import {createVM} from '@ethereumjs/vm';
import {createBlock} from '@ethereumjs/block';
import {Common,Mainnet,Hardfork} from '@ethereumjs/common';
import {Account,createAddressFromString,hexToBytes,bytesToHex} from '@ethereumjs/util';
import {keccak256} from 'ethereum-cryptography/keccak.js';
const f=JSON.parse(readFileSync(process.argv[2]));
for(const [runtime,hash] of [[f.curve_runtime,f.curve_runtime_hash],[f.token_runtime,f.token_runtime_hash]])assert.equal(bytesToHex(keccak256(hexToBytes(runtime))),hash);
const addr=s=>createAddressFromString(s.toLowerCase()),word=n=>BigInt(n).toString(16).padStart(64,'0');
const selector=s=>bytesToHex(keccak256(new TextEncoder().encode(s))).slice(2,10);
const u=r=>BigInt(bytesToHex(r.execResult.returnValue));
for(const c of f.cases){
 const common=new Common({chain:Mainnet,hardfork:Hardfork.Shanghai});
 const vm=await createVM({common}),block=createBlock({header:{number:10000000n,timestamp:2000000000n}},{common});
 const curve=addr(c.address),impl=addr(f.curve_impl),token=addr(c.token),sender=addr('0x0000000000000000000000000000000000000099');
 for(const a of [curve,impl,token,sender])await vm.stateManager.putAccount(a,new Account(0n,a===curve?BigInt(c.native_balance):10n**30n));
 await vm.stateManager.putCode(curve,hexToBytes(c.clone));await vm.stateManager.putCode(impl,hexToBytes(f.curve_runtime));await vm.stateManager.putCode(token,hexToBytes(f.token_runtime));
 for(const [slot,value] of c.storage)await vm.stateManager.putStorage(curve,hexToBytes('0x'+word(slot)),hexToBytes(value));
 await vm.stateManager.putStorage(token,keccak256(hexToBytes('0x'+word(BigInt(c.address))+word(4))),hexToBytes('0x'+word(10n**30n)));
 await vm.stateManager.putStorage(token,hexToBytes('0x'+word(3)),hexToBytes('0x'+word(10n**30n)));
 const call=async(sig,args=[],value=0n,to=curve,shouldRevert=false)=>{
  const before=(await vm.stateManager.getAccount(sender)).balance;
  const r=await vm.evm.runCall({block,to,caller:sender,origin:sender,gasLimit:30000000n,value,data:hexToBytes('0x'+selector(sig)+args.map(word).join(''))});
  assert.equal(Boolean(r.execResult.exceptionError),shouldRevert,`${c.name} ${sig}: ${bytesToHex(r.execResult.returnValue)}`);
  r.nativeDelta=(await vm.stateManager.getAccount(sender)).balance-before;return r;
 };
 const gross=BigInt(c.gross),fee=u(await call('feeBps()')),net=gross-gross*fee/10000n;
 const rawQuote=u(await call('quoteBuy(uint256)',[net]));
 const graduated=u(await call('graduated()'))!==0n;
 const remaining=u(await call('curveSupply()'))-u(await call('tokensSold()'));
 const expected=graduated?rawQuote:(rawQuote<remaining?rawQuote:remaining);
 await call('buy(uint256)',[expected+1n],gross,curve,true);
 const buy=await call('buy(uint256)',[expected],gross);
 assert.equal(u(buy),expected);assert.equal(u(await call('balanceOf(address)',[BigInt(sender.toString())],0n,token)),expected);
 const buyLog=buy.execResult.logs.find(l=>bytesToHex(l[0])===c.address&&bytesToHex(l[1][0])==='0xbeae048c6d270d9469f86cf6e8fedda3c60ad770f16c24c9fc131c8e9a09101d');
 assert.equal(BigInt(bytesToHex(buyLog[1][1])),BigInt(sender.toString()));
 const buyWords=bytesToHex(buyLog[2]).slice(2).match(/.{64}/g).map(v=>BigInt('0x'+v));
 assert.equal(buy.nativeDelta,-buyWords[0]);assert.equal(buyWords[1],expected);
 if(c.name==='graduation_refund'){assert.equal(u(await call('graduated()')),1n);assert.ok(-buy.nativeDelta<gross);}else assert.equal(buy.nativeDelta,-gross);
 const grossOut=u(await call('quoteSell(uint256)',[expected])),netOut=grossOut-grossOut*fee/10000n;
 await call('sell(uint256,uint256)',[expected,netOut],0n,curve,true); // no allowance
 await call('approve(address,uint256)',[BigInt(c.address),expected],0n,token);
 await call('sell(uint256,uint256)',[expected,netOut+1n],0n,curve,true);
 const sell=await call('sell(uint256,uint256)',[expected,netOut]);assert.equal(u(sell),netOut);assert.equal(sell.nativeDelta,netOut);
 const sellLog=sell.execResult.logs.find(l=>bytesToHex(l[0])===c.address&&bytesToHex(l[1][0])==='0x846c37eef631e0943682d87352ec117c20008eb7f425c9b85ac011a6d4774cc0');
 assert.equal(BigInt(bytesToHex(sellLog[1][1])),BigInt(sender.toString()));
 assert.equal(u(await call('balanceOf(address)',[BigInt(sender.toString())],0n,token)),0n);
 console.log(`${c.name}: output=${expected}, net buy=${-buy.nativeDelta}, net sell=${sell.nativeDelta}; strict bounds, recipient, approval and refund passed`);
}
