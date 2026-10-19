import flash.display.Sprite;
import flash.external.ExternalInterface;

class LirCalls extends Sprite {
    @:keep public static function leaf(a:Int, b:Int):Int { return ((a ^ (a << 13)) + b) ^ ((a >>> 17) - (b << 5)); }
    @:keep public static function callLeaf(a:Int, b:Int):Int { return leaf(a, b); }
    @:keep public static function callMath(a:Float, b:Float):Float { return Math.max(a, b); }
    static function main() { var o=new LirCalls(); flash.Lib.current.addChild(o);
        var a:Dynamic=callLeaf, b:Dynamic=callMath, ok=true;
        for(i in 0...1000) {
            var x=i*1103515245+12345, y=(i*7)^(i>>>3);
            if(a(x,y)!=leaf(x,y)) ok=false;
            if(b(x,y)!=Math.max(x,y)) ok=false;
        }
        o.graphics.beginFill(ok?0x00ff00:0xff0000); o.graphics.drawRect(350,250,40,40); o.graphics.endFill();
        if(ExternalInterface.available) ExternalInterface.addCallback("selfTest",function() return ok?"PASS calls":"FAIL calls");
    }
}
