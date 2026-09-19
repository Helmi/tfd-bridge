import {describe, expect, it} from 'vitest';
import {shareFailureCopy} from './shareReplay';

describe('share failure copy',()=>{
  it.each([
    ['not_battle_owner','Engine could not confirm ownership for this battle.'],
    ['recorder_realm_mismatch','Engine could not confirm this replay for the connected account.'],
    ['recorder_account_mismatch','This replay was recorded by another player.'],
    ['capability_revoked','Sharing is no longer authorized. Start again.'],
    ['raw Engine detail','Please try again.'],
  ])('selects safe copy for %s',(code, copy)=>{
    expect(shareFailureCopy(code)).toBe(copy);
  });
});
