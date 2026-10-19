import flash.display.Sprite;
import flash.text.TextField;
class LirLoops extends Sprite {
    static function main() { flash.Lib.current.addChild(new LirLoops()); }
    @:keep static function sum(n:Int):Int { var a = 0; for (i in 0...n) a += i; return a; }
    @:keep static function mix(n:Int, seed:Int):Int { var x=seed; for (i in 0...n) { x ^= x << 13; x ^= x >>> 17; x ^= x << 5; } return x; }
    public function new() {
        super();
        var f:Dynamic=sum, g:Dynamic=mix, good=true;
        var repeats=1;
        var supplied=flash.Lib.current.loaderInfo.parameters.repeats;
        if(supplied!=null){var n=Std.parseInt(supplied);if(n!=null&&n>=1&&n<=100)repeats=n;}
        var started=flash.Lib.getTimer();
        for(repeat in 0...repeats){
        for (n in 0...1000) if (f(n) != Std.int(n*(n-1)/2)) good=false;
        for (n in 0...200) { var x=12345; for (i in 0...n) {x^=x<<13;x^=x>>>17;x^=x<<5;} if(g(n,12345)!=x) good=false; }
        }
        graphics.beginFill(good?0x00ff00:0xff0000);graphics.drawRect(350,250,40,40);graphics.endFill();
        graphics.beginFill((flash.Lib.getTimer()-started)&0xffffff);graphics.drawRect(340,240,4,4);graphics.endFill();
        var t=new TextField();t.width=400;t.text=good?"PASS loops":"FAIL loops";addChild(t);
        if (flash.external.ExternalInterface.available) flash.external.ExternalInterface.addCallback("selfTest",function() return good?"PASS loops":"FAIL loops");
    }
}
