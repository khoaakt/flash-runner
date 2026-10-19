// Integration workload: typed pure integer methods with distinct SSA shapes.
import flash.display.Sprite;
import flash.external.ExternalInterface;
import flash.text.TextField;

class LirArithmetic extends Sprite {
    static function main() { flash.Lib.current.addChild(new LirArithmetic()); }
    @:keep static function add(a:Int, b:Int):Int { return a + b; }
    @:keep static function sub(a:Int, b:Int):Int { return a - b; }
    @:keep static function xor(a:Int, b:Int):Int { return a ^ b; }
    @:keep static function and(a:Int, b:Int):Int { return a & b; }
    @:keep static function or(a:Int, b:Int):Int { return a | b; }
    @:keep static function shl(a:Int, b:Int):Int { return a << b; }
    @:keep static function shr(a:Int, b:Int):Int { return a >> b; }
    @:keep static function ushr(a:Int, b:Int):Int { return a >>> b; }
    @:keep static function neg(a:Int):Int { return -a; }
    @:keep static function bitnot(a:Int):Int { return ~a; }
    @:keep static function mix(a:Int, b:Int):Int { return ((a ^ (a << 13)) + b) ^ ((a >>> 17) - (b << 5)); }
    @:keep static function mul(a:Int, b:Int):Int { return a * b; }
    @:keep static function eq(a:Int, b:Int):Int { return a == b ? 1 : 0; }
    @:keep static function lt(a:Int, b:Int):Int { return a < b ? 1 : 0; }
    @:keep static function ge(a:Int, b:Int):Int { return a >= b ? 1 : 0; }
    @:keep static function select(a:Int, b:Int):Int { return a < b ? a : b; }
    @:keep static function min(a:Int, b:Int, c:Int):Int { return a < b ? (a < c ? a : c) : (b < c ? b : c); }
    @:keep static function abs(a:Int):Int { return a < 0 ? -a : a; }
    @:keep static function clamp(a:Int, lo:Int, hi:Int):Int { return a < lo ? lo : (a > hi ? hi : a); }
    @:keep static function sign(a:Int):Int { return a < 0 ? -1 : (a > 0 ? 1 : 0); }
    public function new() {
        super();
        var functions:Array<Dynamic> = [add, sub, xor, and, or, shl, shr, ushr, mix, mul, eq, lt, ge, select];
        var three:Array<Dynamic> = [min, clamp];
        var signs:Dynamic=sign, absolute:Dynamic=abs;
        for (f in functions) f(123, 7);
        var started = flash.Lib.getTimer();
        var got = 0, want = 0;
        var loops = 20000;
        var supplied = flash.Lib.current.loaderInfo.parameters.loops;
        if (supplied != null) { var parsed = Std.parseInt(supplied); if (parsed != null && parsed >= 1 && parsed <= 2000000) loops = parsed; }
        for (i in 0...loops) {
            var a = i * 1103515245 + 12345, b = (i * 7) ^ (i >>> 3);
            var results = [a+b,a-b,a^b,a&b,a|b,a<<b,a>>b,a>>>b,((a^(a<<13))+b)^((a>>>17)-(b<<5)),a*b,a==b?1:0,a<b?1:0,a>=b?1:0,a<b?a:b];
            for (j in 0...functions.length) { got ^= functions[j](a, b); want ^= results[j]; }
            var c=(a>>>1)^(b<<1);
            got ^= three[0](a,b,c) ^ three[1](a,b,c) ^ signs(a) ^ absolute(a);
            want ^= (a<b?(a<c?a:c):(b<c?b:c)) ^ (a<b?b:(a>c?c:a)) ^ (a<0?-1:(a>0?1:0)) ^ (a<0?-a:a);
        }
        var n:Dynamic = neg, inv:Dynamic = bitnot;
        var unaryOk = true;
        for (i in 0...Std.int(loops / 2)) { var a = i * 1103515245 + 12345; if (n(a) != -a || inv(a) != ~a) unaryOk = false; }
        var ok = got == want && unaryOk;
        var elapsed = flash.Lib.getTimer() - started;
        var hotStart = flash.Lib.getTimer(), hot:Int = 0;
        var typed:Int->Int->Int = mix;
        for (i in 0...loops * 10) hot ^= typed(i, i * 7);
        var hotElapsed = flash.Lib.getTimer() - hotStart;
        var result = (ok ? "PASS" : "FAIL") + ":" + got + ":" + want + ":" + elapsed;
        graphics.beginFill(ok ? 0x00ff00 : 0xff0000); graphics.drawRect(350, 250, 40, 40); graphics.endFill();
        graphics.beginFill(elapsed & 0xffffff); graphics.drawRect(340, 240, 4, 4); graphics.endFill();
        graphics.beginFill(hotElapsed & 0xffffff); graphics.drawRect(330, 240, 4, 4); graphics.endFill();
        var text = new TextField(); text.width = 400; text.text = result; addChild(text);
        if (ExternalInterface.available) ExternalInterface.addCallback("selfTest", function() return result);
    }
}
