import {describe,it,expect} from 'vitest';
import {advancePlayback,endingModel,seekPlayback,videoFrameCount,videoFrameTime} from './ending';
import {estimateLocalVideo,DEFAULT_LOCAL_VIDEO} from '../video/renderSettings';

describe('battle ending timing',()=>{
  it('reaches a millisecond-precise ending from a quantized seek bar',()=>{
    expect(seekPlayback(1128.9,1128.915)).toBe(1128.915);
    expect(seekPlayback(1128.95,1128.915)).toBe(1128.915);
    expect(seekPlayback(1128.85,1128.915)).toBe(1128.85);
    expect(seekPlayback(0,1128.915)).toBe(0);
  });
  it('uses remaining wall time at the battle/end boundary then advances at 1x',()=>{
    expect(advancePlayback(99,0.2,100,10)).toBeCloseTo(100.1);
    expect(advancePlayback(101,0.1,100,40)).toBeCloseTo(101.1);
    expect(advancePlayback(104.4,1,100,10)).toBe(104.5);
  });
  it.each([24,30,60])('adds exactly 4.5 screen seconds at %i fps for every speed',fps=>{
    for(const speed of [1,5,10,20]) {
      const duration=12.345,battleFrames=Math.ceil(duration/speed*fps);
      expect(videoFrameCount(duration,speed,fps)).toBe(battleFrames+4.5*fps);
      expect(videoFrameTime(battleFrames,duration,speed,fps)).toBe(duration);
      expect(videoFrameTime(battleFrames+3*fps,duration,speed,fps)).toBe(duration+3);
      expect(videoFrameTime(battleFrames+4*fps,duration,speed,fps)).toBe(duration+4);
    }
  });
  it('includes ending in the save dialog duration and size estimate',()=>{
    expect(estimateLocalVideo(100,DEFAULT_LOCAL_VIDEO).seconds).toBe(14.5);
  });
});

describe('ending model',()=>{
  it('is null before the end and exists from the end on',()=>{
    expect(endingModel({outcome:'victory',complete:true},-0.01)).toBeNull();
    expect(endingModel({outcome:'victory',complete:true},0)).not.toBeNull();
  });
  it.each([
    ['victory',true,'Victory',[61,177,126]],
    ['defeat',true,'Defeat',[194,65,66]],
    ['draw',true,'Draw',[116,139,149]],
    [undefined,false,'Recording ended',[116,139,149]],
    [undefined,true,'Replay ended',[116,139,149]],
    [undefined,undefined,'Replay ended',[116,139,149]],
  ] as const)('outcome %s, complete %s shows %s',(outcome,complete,title,rgb)=>{
    expect(endingModel({outcome,complete},1)).toMatchObject({title,rgb});
  });
  it('holds for 3 seconds, fades for 1 second, then stays black',()=>{
    for(const [elapsed,alpha] of [[0,0],[2.99,0],[3,0],[3.5,0.5],[4,1],[4.5,1],[60,1]]) {
      expect(endingModel({outcome:'victory',complete:true},elapsed)!.blackAlpha).toBeCloseTo(alpha);
    }
  });
});
