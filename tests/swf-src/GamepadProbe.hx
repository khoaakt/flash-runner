// GameInput probe: lists the game devices and their controls, and paints a green square at (350,250) once device 0 reports AXIS_0 ~ 0.5 and its first button = 1
// (what tools/ext-test.mjs --frame-eval feeds in through a fake navigator.getGamepads); "device 0 reports ... BUTTON_0" means its first button. Needs -swf-version 11.8 for flash.ui.GameInput.
import flash.display.*;
import flash.events.*;
import flash.text.*;
import flash.ui.*;

class GamepadProbe extends Sprite {
	static function main() { flash.Lib.current.addChild(new GamepadProbe()); }
	var out:TextField;
	var gi:GameInput;
	var passed = false;
	function new() {
		super();
		out = new TextField(); out.width = 480; out.height = 330; out.multiline = true; addChild(out);
		gi = new GameInput();
		gi.addEventListener(GameInputEvent.DEVICE_ADDED, function(e:GameInputEvent) { e.device.enabled = true; });
		addEventListener(Event.ENTER_FRAME, tick);
	}
	function tick(_) {
		var s = "supported=" + GameInput.isSupported + " numDevices=" + GameInput.numDevices + "\n";
		var axis0 = Math.NaN, btn0 = Math.NaN;
		for (i in 0...GameInput.numDevices) {
			var d = GameInput.getDeviceAt(i);
			if (d == null) continue;
			d.enabled = true;
			s += "device " + i + ": " + d.name + " controls=" + d.numControls + "\n";
			for (j in 0...d.numControls) {
				var c = d.getControlAt(j);
				if (j < 8) s += "  " + c.id + " = " + c.value + "\n";
				if (i == 0 && c.id == "AXIS_0") axis0 = c.value;
				if (i == 0 && Math.isNaN(btn0) && StringTools.startsWith(c.id, "BUTTON_")) btn0 = c.value;   // Flash numbers the buttons after the axes: BUTTON_4 is the first of a pad with 4 axes
			}
		}
		out.text = s;
		if (!passed && Math.abs(axis0 - 0.5) < 0.02 && btn0 > 0.9) {
			passed = true;
			graphics.beginFill(0x00ff00); graphics.drawRect(350, 250, 40, 40); graphics.endFill();
		}
	}
}
