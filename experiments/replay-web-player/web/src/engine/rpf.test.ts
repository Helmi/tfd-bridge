import { describe, expect, it } from 'vitest';
import { parseRpfDirection } from './rpf';

describe('RPF chat sector parsing', () => {
  it('parses the recorded Bees to Honey announcement', () => {
    expect(parseRpfDirection('RPF: ESE~SE')).toEqual({startBearing:112.5,endBearing:135});
  });

  it('accepts all sixteen compass points and unwraps north', () => {
    const points = ['N','NNE','NE','ENE','E','ESE','SE','SSE','S','SSW','SW','WSW','W','WNW','NW','NNW'];
    for (let index = 0; index < points.length; index++) {
      expect(parseRpfDirection(`RPF: ${points[index]}~${points[(index + 1) % points.length]}`))
        .toEqual({startBearing:index * 22.5,endBearing:(index + 1) * 22.5});
    }
    expect(parseRpfDirection('RPF: NW~NE')).toEqual({startBearing:315,endBearing:405});
  });

  it.each(['~','～','〜','∼'])('accepts separator %s with mixed case and whitespace', separator => {
    expect(parseRpfDirection(` \trPf : eSe ${separator} se \n`)).toEqual({startBearing:112.5,endBearing:135});
  });

  it('normalizes reversed endpoints without changing the indicated shortest sector', () => {
    expect(parseRpfDirection('RPF: SE~ESE')).toEqual({startBearing:112.5,endBearing:135});
    expect(parseRpfDirection('RPF: N~NNW')).toEqual({startBearing:337.5,endBearing:360});
    expect(parseRpfDirection('RPF: NE~NW')).toEqual({startBearing:315,endBearing:405});
  });

  it('retains endpoint order for ambiguous half-turns', () => {
    expect(parseRpfDirection('RPF: N~S')).toEqual({startBearing:0,endBearing:180});
    expect(parseRpfDirection('RPF: S~N')).toEqual({startBearing:180,endBearing:360});
  });

  it.each([
    '', 'ESE~SE', 'RPF ESE~SE', 'RPF: ESE', 'RPF: N~N', 'RPF: NN~NE',
    'RPF: NEE~E', 'RPF: NORTH~NE', 'RPF: 90~135', 'RPF: ESE-SE',
    'RPF: ESE/SE', 'RPF: ESE~~SE', 'RPF: N~NE~E', 'my RPF: ESE~SE',
    'RPF: ESE~SE please', 'RPF: ESE~SE\nfollow me', 'RPF: NE~',
  ])('rejects incomplete or unrelated message %j', message => {
    expect(parseRpfDirection(message)).toBeNull();
  });
});
