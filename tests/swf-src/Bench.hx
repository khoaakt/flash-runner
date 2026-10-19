// CPU benchmark movie: pure ActionScript work (no host calls except Date/timer reads), results drawn on the stage so any engine can be compared.
import flash.display.*;
import flash.text.*;
import flash.utils.*;

class Bench extends Sprite {
	static function main() { flash.Lib.current.addChild(new Bench()); }
	var out:TextField;
	function new() {
		super();
		out = new TextField(); out.width = 480; out.height = 300; out.multiline = true; addChild(out);
		flash.Lib.current.stage.addEventListener(flash.events.Event.ENTER_FRAME, once);
	}
	var done = false;
	function once(_) {
		if (done) return; done = true;
		var t:Int; var s = "";
		function lap(name:String, f:Void->Dynamic) { t = flash.Lib.getTimer(); var r = f(); s += name + "=" + (flash.Lib.getTimer() - t) + "ms  (" + r + ")\n"; out.text = s; }
		lap("int_loop", function() { var x:Int = 12345, a:Int = 0; for (i in 0...20000000) { x ^= x << 13; x ^= x >>> 17; x ^= x << 5; a += x & 255; } return a; });
		lap("float_math", function() { var a = 0.0; for (i in 0...3000000) { var f = i * 0.001; a += Math.sqrt(f) + Math.sin(f) + Math.floor(f * 1.7) + Math.ceil(f * 0.3) + Math.round(f); } return Std.int(a); });
		lap("array_sort", function() { var arr:Array<Int> = []; var x = 1; for (i in 0...300000) { x = (x * 1103515245 + 12345) & 0x7fffffff; arr.push(x); } arr.sort(function(a, b) return a - b); return arr[1000]; });
		lap("date", function() { var a = 0.0; for (i in 0...200000) { var d = new Date(2024, i % 12, 1 + i % 28, i % 24, 0, 0); a += d.getTime() % 1000 + d.getDay(); } return Std.int(a); });
		lap("xml", function() { var sb = new StringBuf(); sb.add("<r>"); for (i in 0...6000) sb.add('<i id="$i" n="name$i"><v>$i</v></i>'); sb.add("</r>"); var x = new flash.xml.XML(sb.toString()); return x.children().length() + x.descendants("v").length() + x.toXMLString().length; });
		lap("bytearray", function() { var b = new ByteArray(); for (i in 0...1000000) b.writeInt(i * 7); b.position = 0; var a = 0; for (i in 0...1000000) a ^= b.readInt(); return a; });
		lap("string", function() { var a = 0; for (i in 0...100000) { var str = "item" + i + ":" + (i * 3); a += str.length + str.indexOf(":") + str.split(":").length; } return a; });
		s += "DONE\n"; out.text = s;
	}
}
