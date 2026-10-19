import flash.display.Sprite;
import flash.external.ExternalInterface;

class LirMemory extends Sprite {
    public var first:Int;
    public var second:Int;
    public function new(a:Int, b:Int) { super(); first = a; second = b; }
    @:keep public function getFirst():Int { return first; }
    @:keep public function getSum():Int { return first + second; }
    @:keep public function addTo(a:Int):Int { return first + a; }
    @:keep public function setFirst(a:Int):Int { first = a; return first; }
    @:keep public function bump(a:Int):Int { first += a; return first; }
    @:keep public function scramble(a:Int):Int {
        var x=first;
        first=((x^(x<<13))+a)^((x>>>17)-(a<<5));
        return first;
    }
    @:keep public function exchange(a:Int):Int {
        var saved=first;
        first=second;
        second=saved+a;
        return first+second;
    }
    static function main() {
        var o = new LirMemory(123, -456);
        flash.Lib.current.addChild(o);
        var a:Dynamic = o.getFirst, b:Dynamic = o.getSum, c:Dynamic = o.addTo;
        var d:Dynamic = o.setFirst, e:Dynamic = o.bump;
        var scramble:Dynamic=o.scramble, exchange:Dynamic=o.exchange;
        a(); b(); c(0); d(123); e(0); scramble(0); d(123); exchange(0); d(123); o.second=-456;
        var repeats=1, supplied=flash.Lib.current.loaderInfo.parameters.repeats;
        if(supplied!=null){var parsed=Std.parseInt(supplied);if(parsed!=null && parsed>=1 && parsed<=100)repeats=parsed;}
        var ok = true;
        var started=flash.Lib.getTimer();
        for(repeat in 0...repeats){
        for (i in 0...20000) {
            if (a() != o.first || b() != o.first + o.second || c(i) != o.first + i) ok = false;
            if (d(i) != i || o.first != i || e(-7) != i - 7 || o.first != i - 7) ok = false;
            var x=o.first,want=((x^(x<<13))+i)^((x>>>17)-(i<<5));
            if(scramble(i)!=want || o.first!=want)ok=false;
            var saved=o.first, exchanged=o.second;
            if(exchange(i)!=exchanged+(saved+i) || o.first!=exchanged || o.second!=saved+i)ok=false;
        }
        }
        var elapsed=flash.Lib.getTimer()-started;
        o.graphics.beginFill(ok ? 0x00ff00 : 0xff0000); o.graphics.drawRect(350,250,40,40); o.graphics.endFill();
        o.graphics.beginFill(elapsed&0xffffff);o.graphics.drawRect(340,240,4,4);o.graphics.endFill();
        if (ExternalInterface.available) ExternalInterface.addCallback("selfTest",function() return ok ? "PASS memory" : "FAIL memory");
    }
}
