# Aeglos Systems — Internal Product Catalogue
**CONFIDENTIAL — INTERNAL USE ONLY**
*Last updated: 2026-03-19*

---

## 1. AEGLOS ANALYTICS
*AI-powered defense intelligence platform*

---

### 1.1 Analytics Core
**Description:** The foundational on-device inference engine. Runs quantized LLMs (8B parameter class) directly on edge hardware with no cloud dependency. Ingests multi-source intelligence feeds (SIGINT, HUMINT reports, imagery metadata) and surfaces ranked threat assessments in plain language. Designed for battalion-level S2 shops and forward-deployed analysts.

**Source Materials:**
- **Compute hardware:** NVIDIA Jetson AGX Orin (60 TOPS) or Qualcomm QCS8550 for embedded deployments
- **Model weights:** Qwen3-8B / Mistral-class GGUF — direct from HuggingFace or fine-tuned in-house on cleared datasets
- **Chassis/enclosure:** Themis Computer (Freemont, CA) or Mercury Systems (Andover, MA) — both have existing defense chassis lines
- **Integration pathway:** DI2E Framework, NATO STANAG 4559 feed compatibility

---

### 1.2 Threat Vector
**Description:** Real-time threat assessment module that layers geospatial data, pattern-of-life baselines, and anomaly detection. Outputs probability-weighted threat scores per grid square updated on configurable intervals (down to 30s). Feeds directly into common operating picture (COP) overlays via TAK plugin.

**Source Materials:**
- **Mapping/geo engine:** Esri ArcGIS Defense (ESRI Federal, Redlands, CA) or open-source alternative via GeoServer
- **TAK integration:** TAK Product Center (government-operated, free SDK) for ATAK/WinTAK plugin development
- **Satellite imagery feeds:** Maxar Technologies (Westminster, CO) or Planet Labs — both offer government API contracts
- **Hardware acceleration:** AMD Instinct MI300X for server-side deployments; Hailo-8 for embedded

---

### 1.3 Pattern Forge
**Description:** Predictive modeling suite for pattern-of-life (POL) analysis. Ingests historical movement data, communication intercept metadata, and open-source indicators to build behavioral profiles and forecast actor movements. Outputs 24/48/72-hour probability overlays.

**Source Materials:**
- **Data pipeline:** Apache Kafka + Apache Spark cluster (open source, self-hosted)
- **Graph analysis engine:** Neo4j Enterprise (Neo4j Inc., San Mateo, CA) for relationship mapping
- **OSINT aggregation:** Babel Street (Reston, VA) or Recorded Future (Somerville, MA) — both have government contracts
- **Infrastructure:** Dell PowerEdge servers with STIG-compliant RHEL 9; or AWS GovCloud for cloud-optional deployments

---

### 1.4 Command Lens
**Description:** Commander-facing decision support dashboard. Aggregates output from Analytics Core, Threat Vector, and Pattern Forge into a single situational awareness interface. Designed for non-technical end users — touch-optimized, NATO APP-6 symbology, voice query ("what is the threat level in grid 4QFJ") via on-device NLP.

**Source Materials:**
- **Display hardware:** Getac F110 rugged tablet or Panasonic Toughbook 33 — both MIL-STD-810H rated
- **Symbology library:** MIL-STD-2525D compliant icon sets (open standard, DoD-published)
- **Voice/NLP layer:** Built on Aeglos OS Numenor engine (in-house); no external dependency
- **Enclosure/mount solutions:** RAM Mounts (Seattle, WA) for vehicle/aircraft integration

---
---

## 2. ARIES SERIES
*Professional-grade breaching equipment*

---

### 2.1 Aries-1 Primary Breacher
**Description:** Heavy-duty door-breaching ram, 35 lbs, aircraft-grade 6061 aluminum tube with hardened steel strike face. 24" effective stroke. Designed for two-operator use on standard commercial and military door assemblies. Non-sparking strike face option available for fuel-sensitive environments.

**Source Materials:**
- **Steel fabrication:** Multiple domestic CNC shops — primary candidate: Metal Storm Inc. (Cincinnati, OH) or Dynamic Metal Services (Dallas, TX)
- **Aluminum stock:** Kaiser Aluminum (Franklin, KY) — T6 6061 bar stock, domestic supply chain
- **Strike face hardening:** Nitriding or Cerakote coating via Cerakote (White City, OR)
- **Reference competitors for spec benchmarking:** Blackhawk Battering Ram, Specter Gear, TFP (Tactical Fieldcraft Products)

---

### 2.2 Aries-2 Compact CQB
**Description:** Single-operator compact breaching tool, 14 lbs, designed for confined spaces, vehicle entries, and covert operations. Collapsible handle reduces pack profile to 18". Integrated pry wedge on the heel for frame separation. Compatible with standard PALS/MOLLE large utility pouch.

**Source Materials:**
- **Fabrication:** Same shop as Aries-1; tooling amortized across the line
- **Collapsible handle mechanism:** Similar to ASP baton mechanisms — contact ASP Inc. (Appleton, WI) for OEM component licensing, or source from Galls/Safariland supply chain
- **Surface finish:** Type III hard anodize for aluminum; Cerakote matte black for steel components
- **Carry solution:** Custom MOLLE pouch — Eagle Industries (Fenton, MO) or Tactical Tailor (Lakewood, WA) for cut-and-sew

---

### 2.3 Aries-3 Hydraulic Assist
**Description:** Hydraulic-assisted breaching spreader for reinforced doors, vehicle doors, and barricaded entries. 10-ton spread force, 6" jaw opening, powered by compact hand pump with 18" hose. Designed to defeat multi-point locking systems and armored door frames where ram force alone is insufficient.

**Source Materials:**
- **Hydraulic components:** Enerpac (Menomonee Falls, WI) — industry standard for compact hydraulic tooling; available for OEM integration
- **Jaw fabrication:** 4140 chromoly steel, machined and heat-treated; vendors include Precision Castparts (Portland, OR) or local tool-and-die shops
- **Seals/hoses:** Parker Hannifin (Cleveland, OH) — standard hydraulic seals, high availability
- **Carry case:** Pelican Products (Torrance, CA) — 1510 or 1560 case with custom foam cut

---

### 2.4 Aries-4 Hooligan Pro
**Description:** Multi-function forcible entry tool combining halligan bar, flat bar, and striking surface in a single 30" forged steel tool. Fork, adze, and pike geometry optimized for inward-swinging, outward-swinging, and sliding door assemblies. Compatible with Aries-1 for battering ram combination techniques.

**Source Materials:**
- **Forging:** Bharat Forge (US operations in Sandusky, OH) or Pacific Forge (City of Industry, CA) — both capable of drop-forged steel tool production
- **Steel spec:** 4340 alloy steel, hardness 50-55 HRC at working surfaces; normalized shank for shock resistance
- **Heat treatment:** Atmosphere-controlled furnace — Solar Atmospheres (Souderton, PA)
- **Reference product for spec:** Leatherhead Tools Pro-Bar, Fire Hooks Unlimited — study these for geometry

---
---

## 3. DURENDAL
*Premium tactical knives and edged tools*

---

### 3.1 Durendal MK-1 Fixed Blade
**Description:** 8" drop-point fixed blade, primary tactical/utility knife. Full tang, 0.2" thick CPM-3V tool steel, satin finish with optional Cerakote. 13.5" overall length. G10 scales with finger groove index. Kydex sheath with MOLLE mount and adjustable retention. Made in USA.

**Source Materials:**
- **Blade steel:** Crucible Industries CPM-3V (Syracuse, NY) — premium tool steel, excellent toughness for field use
- **Handle scales (G10):** Norplex-Micarta (Postville, IA) — domestic G10 and Micarta supply
- **Kydex sheath:** Kydex LLC (Lincolnshire, IL) — 0.093" Kydex sheet stock
- **Custom knife makers:** Chris Reeve Knives (Boise, ID), or production run via ESEE Knives (Gallatin, TN) who do custom OEM runs; also consider Bark River Knives (Escanaba, MI)
- **Cerakote application:** Cerakote (White City, OR) or local licensed applicators

---

### 3.2 Durendal MK-2 Combat Folder
**Description:** 3.5" clip-point folding blade, CPM-S35VN stainless steel. IKBS ball bearing pivot, frame lock, G10 front scale, titanium back spacer. Tip-up/tip-down reversible pocket clip. Designed for one-hand deployment and gloved operation. No assisted opener — intentional, for jurisdictional flexibility.

**Source Materials:**
- **Production OEM:** Zero Tolerance Knives (Tualatin, OR) — they produce OEM runs for branded buyers; or Benchmade (Oregon City, OR) — OEM program exists for volume orders
- **Blade steel:** Crucible CPM-S35VN — superior corrosion resistance for maritime/humid environments
- **Pivot hardware:** IKBS (Ikoma Korth Bearing System) — license from Korth Design or source equivalent ceramic bearings through industrial suppliers
- **Titanium components:** ATI (Allegheny Technologies, Pittsburgh, PA) for titanium sheet/bar

---

### 3.3 Durendal Field Knife
**Description:** 5.5" clip-point multi-purpose field knife. Slightly thinner grind than MK-1 for food prep, cordage cutting, and general field use alongside primary tactical blade. 1095 high-carbon steel (simple, sharpenable in the field), black epoxy powder coat. Leather or Kydex sheath options. Full tang with lanyard hole.

**Source Materials:**
- **Steel:** 1095 high-carbon — widely available from Steel Technologies (Louisville, KY) or Service Center Network
- **Production:** Ka-Bar Knives (Olean, NY) — they have a strong OEM history; ESEE Knives (Gallatin, TN) for custom production runs; both experienced with 1095
- **Powder coat:** Local industrial coating shops; Prismatic Powders (Mesa, AZ) for color-matched matte finish
- **Leather sheath:** Hermann Oak Leather (St. Louis, MO) for top-grain vegetable-tanned leather

---

### 3.4 Durendal Boot Knife
**Description:** 3.75" double-edge dagger, skeletonized 440C stainless frame, wrapped handle with 550 cord or Micarta slabs. Designed for concealment in boot sheath or ankle rig. Minimal profile, 6.75" OAL, 3 oz. Last-ditch defensive tool. Ships with nylon ankle sheath with elastic retention.

**Source Materials:**
- **Blade production:** Cold Steel (Ventura, CA) has OEM experience and produces similar geometry; alternatively SOG Specialty Knives (Lynnwood, WA)
- **Steel:** 440C or AUS-8 for corrosion resistance and cost efficiency at this price point
- **Paracord wrap:** Atwood Rope Manufacturing (Pittsfield, NH) — mil-spec Type III 550 cord, made in USA
- **Ankle sheath fabrication:** Uncle Mike's (a Michaels of Oregon brand) produces generic sheaths; or custom nylon work via Tactical Tailor (Lakewood, WA)

---
---

## 4. WEATHERTOP RECON
*Advanced surveillance and reconnaissance systems*

---

### 4.1 Weathertop Long Eye
**Description:** 20-60x80mm spotting scope with ED glass objective, fully multi-coated. Mil-dot reticle, dual-speed focuser, armored rubber housing. Designed for long-range observation posts, sniper team use, and forward observer missions. Includes mil/MOA ranging capability. MIL-STD-810H shock/vibration rated.

**Source Materials:**
- **Optics OEM:** Nightforce Optics (Orofino, ID) — they produce mil-spec glass and have done custom OEM; Leupold & Stevens (Beaverton, OR) — Mark 4/5 series, strong government contract history
- **Lens elements/coating:** SCHOTT AG (US operations) for specialty glass; or Ohara Corp (US subsidiary, Branchburg, NJ)
- **Chassis machining:** 6061-T6 aluminum — any precision CNC shop; consider Surefire's machining contractor network
- **Tripod/mount:** Outdoorsmans (Phoenix, AZ) or Leupold tripod systems for spotting scope heads

---

### 4.2 Weathertop Thermal
**Description:** Clip-on/standalone thermal observation monocular. 640x480 VOx microbolometer, 25mm germanium lens, 30Hz refresh, NETD <40mK. White-hot/black-hot/LUTS display modes. On-device video recording, image capture, and Bluetooth export to TAK-enabled device. Battery life 8+ hours via 18650 cells.

**Source Materials:**
- **Thermal core (microbolometer):** FLIR/Teledyne (Wilsonville, OR) — FLIR Boson 640 core; they sell to integrators under U.S. export compliance
- **Germanium optics:** II-VI Incorporated (now Coherent Corp, Pittsburgh, PA) — primary supplier of defense-grade germanium lenses
- **Housing/electronics integration:** DRS Technologies (now Leonardo DRS, Arlington, VA) or Sierra Nevada Corporation for full integration
- **Alternative path:** Source complete units from FLIR Systems or L3Harris Warrior Systems and rebrand under OEM arrangement (requires volume commitment, ~500 units min)

---

### 4.3 Weathertop UGS
**Description:** Unattended Ground Sensor (UGS) package. Seismic/acoustic detection node with PIR trigger, optional magnetic anomaly detector for vehicle detection. 90-day battery life on D-cell pack. Encrypted 900MHz mesh radio for multi-sensor network. Buried or surface-deployable. Waterproof IP67.

**Source Materials:**
- **Seismic sensors:** PCB Piezotronics (Depew, NY) — they supply seismic transducers to DoD programs
- **Radio mesh module:** Rajant Corporation (Malvern, PA) — BreadCrumb mesh radio modules are established in defense UGS programs
- **PIR sensors:** Parallax/Panasonic PIR modules for prototype; Murata for production
- **Enclosure:** Pelican (Torrance, CA) Micro Case series, or custom injection-molded ABS/PC housing from Nypro Defense or Integer Holdings
- **Reference programs:** L3Harris UnAttended Ground Sensors, Textron Systems Unattended Ground Sensors — study form factor

---

### 4.4 Weathertop Eye-in-the-Sky
**Description:** Man-packable Group 1 UAS (under 20 lbs MTOW) for persistent surveillance. Fixed-wing VTOL hybrid, 90-minute endurance, EO/IR gimbaled payload, encrypted C2 link. Auto-return-to-home on signal loss. Ground control station runs on Getac rugged tablet with TAK integration. Recoverable via belly-landing on prepared surface.

**Source Materials:**
- **Airframe:** Shield AI (San Diego, CA) or Joby Aviation's defense division; alternatively FLIR's SkyRaider UAS platform
- **EO/IR payload:** DJI Zenmuse XT2 (if export-compliant) or Trakus/L3Harris gimbals for defense-grade
- **C2 link encryption:** L3Harris ATAK-compatible encrypted datalink; or Persistent Systems (New York, NY) MPU5 radio
- **Battery:** Intelligent Energy fuel cell (UK company with US operations) for extended endurance; or custom LiPo from EaglePicher Technologies (Joplin, MO)

---
---

## 5. BALOR MEDICAL
*Combat medical kits and trauma equipment*

---

### 5.1 Balor IFAK
**Description:** Individual First Aid Kit per TCCC (Tactical Combat Casualty Care) guidelines. Contents: CAT tourniquet, compressed gauze x2, chest seal pair, trauma shears, nasopharyngeal airway with lube, nitrile gloves. Packaged in MOLLE-compatible blow-out pouch with one-handed rip-strip access. IFAK insert replaceable independently of pouch.

**Source Materials:**
- **Tourniquet:** North American Rescue (Greer, SC) CAT Gen-7 — the DoD standard; available for bulk OEM with custom branding at volume
- **Chest seal:** SAM Medical (Tualatin, OR) SAM Chest Seal; or Hyfin (NAR) — both TCCC recommended
- **Gauze:** Dynarex (Orangeburg, NY) or Medline Industries compressed gauze — FDA-registered, available in bulk
- **Pouch manufacturing:** North American Rescue also produces pouches; or Tactical Tailor (Lakewood, WA) for cut-and-sew custom MOLLE pouch production

---

### 5.2 Balor TQ-7
**Description:** Combat application tourniquet, windlass design, 1.5" wide band, auto-locking clip. One-hand self-application capable. Printed with application instructions on band. High-visibility markings on windlass. Rated to occlude brachial and femoral arteries in <60 seconds. NSN-listed equivalent to CAT Gen-7.

**Source Materials:**
- **OEM manufacturer:** North American Rescue (Greer, SC) — they manufacture the CAT under government contract; approach for branded private-label program
- **Alternative:** SOFTT-W from Tactical Medical Solutions (Anderson, SC) — second DoD-approved windlass TQ; also available for OEM discussion
- **Band material:** Mil-spec nylon webbing from Murdock Webbing (Central Falls, RI) or Carolina Narrow Fabric
- **Windlass rod:** Delrin acetal polymer rod stock — widely available from plastics distributors (Curbell Plastics)

---

### 5.3 Balor Hemostatic Dressing
**Description:** Combat gauze impregnated with kaolin hemostatic agent. 3" x 4 yards, z-fold packed for one-hand wound packing. Effective on junctional wounds (groin, axilla, neck). Sterile, individually sealed, 5-year shelf life. Replaces standard compressed gauze as primary hemostatic intervention.

**Source Materials:**
- **QuikClot Combat Gauze:** Z-Medica (Wallingford, CT) — original kaolin-based hemostatic gauze; government contract holder; approach for private-label/OEM supply agreement
- **Alternative:** HemCon ChitoGauze from HemCon Medical Technologies (Portland, OR) — chitosan-based alternative with similar performance data
- **Packaging:** Sealed Air Corporation (Charlotte, NC) for medical-grade sterile packaging; or Ampac (Cincinnati, OH)
- **Regulatory path:** Both source products are FDA 510(k) cleared; Aeglos would rebrand under existing clearances via supply agreement

---

### 5.4 Balor Trauma Bag
**Description:** Extended care trauma bag for medic/18D use. 1,000D Cordura exterior, drag handle, dual-access zipper, internal divider system. Sized for 72-hour supply of consumables. Contents per MARCH protocol: airway management (NPA, supraglottic), hemorrhage control (TQs x4, chest seals x4, gauze x8, Israeli bandage x4), circulation (IV start kit, 1L saline x2, pressure infuser), hypothermia prevention (SOF Tactical Litter, emergency blanket).

**Source Materials:**
- **Bag fabrication:** Chinook Medical Gear (Durango, CO) — they manufacture custom trauma bags for special operations; or North American Rescue for full kit assembly
- **1000D Cordura:** Invista (Kennesaw, GA) — domestic Cordura supply; specify Multicam or coyote tan for mil-spec color
- **IV supplies:** Baxter International (Deerfield, IL) or B. Braun Medical (Bethlehem, PA) for IV fluids and start kits
- **Hypothermia kit:** SOF Tactical Litter from MyMedic or Rescue Essentials; emergency blankets from Survive Outdoors Longer (SOL)

---
---

## 6. GREY COMPANY TACTICAL
*Elite plate carriers and load-bearing equipment*

---

### 6.1 Grey Company Plate Carrier
**Description:** Scalable plate carrier accepting 10x12" SAPI/ESAPI plates. Cummerbund with side-plate pockets (6x8"). Full PALS/MOLLE front/back/sides. Coyote tan, ranger green, multicam color options. Drag handle. Low-profile when stripped, full mission configuration with cummerbund and shoulder pads. Compatible with all COTS accessories.

**Source Materials:**
- **Cut-and-sew manufacturing:** Crye Precision (Brooklyn, NY) — they OEM for SOCOM directly; approach for contract manufacturing; or Eagle Industries (Fenton, MO) — large-scale cut-and-sew, existing DoD contracts
- **Fabric:** 500D Cordura from Invista; MOLLE webbing from Murdock Webbing (Central Falls, RI)
- **Hardware:** AustriAlpin (Austria, with US distribution) or ITW Nexus — mil-spec buckles, Fastex clips, D-rings
- **Plates (to bundle):** Highcom Security (Hamilton, OH) — Level III/IV UHMWPE and ceramic plates; or Hesco Armor (Dyneema-based)

---

### 6.2 Grey Company Battle Belt
**Description:** Modular 2" duty/battle belt system. Inner/outer belt with velcro interface. Outer belt: full PALS/MOLLE, reinforced with plastic stiffener. Accepts pistol holster, mag pouches, IFAK, dump pouch. Fits 32"-46" waist. Cobra buckle closure. Can run standalone or integrated with plate carrier via belt-to-PC loop connectors.

**Source Materials:**
- **Cobra buckle:** AustriAlpin Cobra buckle — the industry standard; US distribution through US Tactical Supply
- **Belt manufacturing:** Blue Force Gear (Pooler, GA) — they manufacture belts and MOLLE accessories with domestic cut-and-sew; or HSGI (High Speed Gear Inc., Matthews, NC)
- **Stiffener material:** Kydex 0.08" for inner stiffener; or Boltaron thermoplastic from Boltaron Inc. (Newcomerstown, OH)
- **Reference belts:** Spiritus Systems, Ferro Concepts, Blue Force Gear Helium Whisper — study for geometry and tolerances

---

### 6.3 Grey Company Chest Rig
**Description:** Standalone lightweight chest rig for high-mobility operations where plate carrier is not mission-appropriate. Six triple-mag panel (M4/AK configurable), admin pouch, side cummerbund with two utility pouches. Total weight under 1.5 lbs empty. Designed for recon, HALO, and dismounted patrol where speed overrides protection.

**Source Materials:**
- **Fabrication:** Spiritus Systems (Phoenix, AZ) produces comparable geometry; approach for OEM or study as engineering reference; or have Tactical Tailor (Lakewood, WA) produce from provided patterns
- **Mag retention:** Shock cord + Kydex welt, or hook-and-loop — standard in the industry; no proprietary components
- **Materials:** 500D Cordura + mil-spec webbing (same supply chain as plate carrier above)
- **Buckles:** Side-release buckles from ITW Nexus SR-10 series — available through US Tactical Supply or direct from ITW

---

### 6.4 Grey Company Assault Pack
**Description:** 24-hour assault pack, 1,500 cubic inch main compartment, hydration-compatible (3L bladder sleeve), admin panel front, side compression straps, low-profile shoulder straps for PC compatibility. Padded laptop sleeve. MOLLE front and sides. Drag handle. Can be worn standalone or lashed to plate carrier back panel.

**Source Materials:**
- **Pack manufacturing:** Mystery Ranch (Bozeman, MT) — premium DoD pack manufacturer with YOKE suspension system; they OEM for military programs; or Eberlestock (Boise, ID) — makes mil-spec packs with existing DoD contracts
- **Frame sheet (if included):** High-density polyethylene (HDPE) sheet from Curbell Plastics; or carbon fiber sheet from Rock West Composites (Salt Lake City, UT)
- **Zippers:** YKK (Macon, GA — US operations) #10 and #5 AquaGuard series for water resistance
- **Hydration bladder:** Source Tactical (Israeli origin, US distribution) or Osprey for OEM bladder supply

---
---

## 7. GALAD LIGHTING
*Professional tactical illumination systems*

---

### 7.1 Galad Torch-1 Handheld
**Description:** 1,200-lumen tactical flashlight, single 21700 cell, 5 modes (1/25/250/1200 lm + strobe), dual tail/side switch, type III hard-anodized 6061 aluminum, 1-meter drop rated. Flat bezel for tail-standing. Anti-roll body. Compatible with standard holsters and drop pouches. Runtime: 1.5 hours high, 18 hours low.

**Source Materials:**
- **LED emitter:** Luminus SST-70 or Cree XHP70.3 — order direct from Luminus Devices (Sunnyvale, CA) or Cree (Durham, NC) for production quantities
- **Driver electronics:** Custom PCB designed to spec; PCB fabrication via Sanmina (San Jose, CA) or API Technologies for defense-grade boards
- **Machined body:** 6061-T6 aluminum CNC turned; shops include Proto Labs (Maple Plain, MN) for prototyping, then scale to regional machine shops
- **Alternative — OEM path:** Streamlight (Eagleville, PA) has OEM/private-label programs; Fenix Lighting (US entity in San Jose, CA) also OEMs with branding for volume buyers

---

### 7.2 Galad Weapon Light
**Description:** Rail-mounted weapon light, 1,000 lumens, CR123A or rechargeable 16340 cell. Picatinny and M-LOK mount included. Ambidextrous tail cap with remote pressure switch port. Strobe mode via double-tap. Polycarbonate lens, O-ring sealed, IPX7 waterproof. Reverse polarity and high-voltage protected. Fits AR-platform and most pistol rails.

**Source Materials:**
- **OEM source:** Cloud Defensive (Mesa, AZ) produces premium weapon lights and has engaged OEM discussions previously; or Modlite Systems (Scottsdale, AZ) — premium COTS that can serve as engineering benchmark
- **Surefire:** They do not OEM, but their EDCL2-T and Scout line are the performance standard to match
- **Mount hardware:** Arisaka Defense (Lawndale, CA) produces quality rail mounts; or procure from Unity Tactical for modular mount compatibility
- **Switch cables:** Surefire-compatible remote switch cable from Cloud Defensive or Malkoff Devices (Suwanee, GA)

---

### 7.3 Galad NVG Headlamp
**Description:** Multi-mode headlamp with dual-output heads — visible white (300 lm) and IR (850nm, NVG-compatible). Separate switches for visible/IR to prevent accidental white-light compromise. Red and green aux LEDs. Single 18650 cell, 8-hour runtime on low. Mil-spec elastic headband, helmet mount compatible (NOROTOS/Wilcox footprint). Waterproof IPX6.

**Source Materials:**
- **IR LED source:** OSA Opto Light (Germany, US distribution) for 850nm high-power IR LEDs; or Marubeni (US) distributes Epitex IR emitters
- **Visible LED:** Same Luminus/Cree source as Torch-1
- **Helmet mount interface:** NOROTOS (Phoenix, AZ) aluminum J-arm mount — they supply to NVG manufacturers; or Wilcox Industries (Newington, NH) — both offer interface hardware
- **OEM headlamp path:** Petzl (US subsidiary, Clearfield, UT) has tactical line (Tactikka) and explores OEM; or Energizer Defense (St. Louis, MO) for volume OEM

---

### 7.4 Galad IR Beacon
**Description:** Infrared marking beacon for personnel and equipment marking (CAS coordination, CSAR, LZ marking). Dual-mode: constant-on and 1Hz strobe. Visible to standard Gen-2/3 NVGs and FLIR. Magnetic base for equipment attachment, spike base for ground marking. Waterproof, crush-resistant. Battery life 40 hours constant, 200 hours strobe. 9V battery.

**Source Materials:**
- **IR beacon core:** Adventure Lights (Granbury, TX) manufactures IR strobes and sells to DoD; approach for private-label arrangement
- **Alternative:** ACR Electronics (Palm City, FL) produces emergency beacons with IR capability; strong DoD supply chain history
- **Enclosure:** Custom injection-molded from Protolabs or Rodon Group (Hatfield, PA) — both capable of mil-spec plastics
- **Magnet base:** Bunting Magnetics (Newton, KS) for rare-earth magnet assemblies; ground spike from metal stamping vendor

---
---

## 8. CORVUS EDC
*Premium everyday carry multi-tools and accessories*

---

### 8.1 Corvus Multi-Tool Pro
**Description:** Full-size multi-tool, 17 implements: pliers, wire cutter (replaceable), serrated/straight blade, three screwdrivers, can/bottle opener, file, awl, bit driver (includes 8 hex bits). 420HC stainless steel construction, black oxide finish. Locking main blade and all tools. Snap nylon sheath with belt loop and MOLLE attachment.

**Source Materials:**
- **OEM manufacturer:** Leatherman Tool Group (Portland, OR) — they have an OEM/private-label program for volume buyers (typically 500+ units minimum); Wave+ and Surge platforms as base
- **Alternative:** Victorinox (Switzerland, with US entity in Monroe, CT) — they also OEM Swiss Army tools for private label
- **Alternative domestic:** SOG Specialty Knives (Lynnwood, WA) has OEM history
- **Bit set:** Wiha Tools (Monticello, MN) — quality hex bit sets; or Bondhus (Monticello, MN) for domestic bits
- **Sheath:** Eagle Industries (Fenton, MO) or Tactical Tailor for MOLLE-compatible sheaths

---

### 8.2 Corvus Compact Keychain Tool
**Description:** 10-implement keychain multi-tool, 2.75" closed length, 2oz. Implements: mini flat/Phillips screwdriver, can opener, bottle opener, file, scissors, small blade, tweezers, toothpick, key ring. 420 stainless. Polished handles or G10 option. Fits on standard key ring without bulk. TSA-friendly (no lockable blade exceeding 2.36").

**Source Materials:**
- **OEM source:** Victorinox Classic SD is the category-defining platform — approach US entity for OEM
- **Domestic alternative:** True Utility (UK company, US distribution) makes compact keychain tools and has discussed OEM arrangements
- **Custom option:** Small run from American Tomahawk Company (LaGrange, GA) or similar specialty tool shop willing to do low-volume custom
- **Key ring hardware:** O-Mega Industries (Los Angeles, CA) for stainless key rings and split rings in bulk

---

### 8.3 Corvus Tactical Pen
**Description:** Tactical writing instrument doubling as defensive tool and glass breaker. Aircraft-grade 6061 aluminum body, knurled grip, hardened tungsten carbide tip (glass breaker). Accepts standard Parker-style refills. Can be used as pressure point tool, kubotan, or in extremis defensive instrument. Black anodize. Pocket clip. 5.5" length, 1.2oz.

**Source Materials:**
- **CNC machining:** Any precision aluminum CNC shop — body is a simple turned part; Proto Labs for prototyping, then regional machine shops at scale
- **Tungsten carbide insert:** Kennametal (Pittsburgh, PA) or Sandvik Coromant (US distribution, Fair Lawn, NJ) — both supply carbide inserts to industry
- **Refills:** Parker Pen Company — Quinkflow medium refills widely distributed; or Fisher Space Pen (Boulder City, NV) pressurized refills for field reliability
- **Anodizing:** Local Type II/III anodize shops; or AeroSurface Technologies (multiple US locations)
- **Reference product:** Benchmade Damned pen, Gerber Impromptu — study as benchmarks

---

### 8.4 Corvus RFID Wallet
**Description:** Minimal tactical wallet, 6061-T6 aluminum frame with carbon fiber face plates, stainless steel card retainer band. Holds 4-8 cards + folded bills. RFID/NFC blocking by enclosure geometry (no separate foil layer required — Faraday cage by design). Elastic band for bills, quick-eject mechanism for top card. 3oz, fits standard back pocket.

**Source Materials:**
- **Aluminum extrusion/machining:** Bonnell Aluminum (Newnan, GA) for extrusion; finish machining at regional shops
- **Carbon fiber face plates:** Rock West Composites (Salt Lake City, UT) or DragonPlate (Elbridge, NY) — prepreg carbon fiber panels available in custom cut sizes
- **Stainless band:** Small spring steel or stainless flat stock from Service Center Metals; laser cut to spec
- **RFID blocking validation:** Test per ISO/IEC 14443 and 15693; EMC testing labs include NTS (Fullerton, CA) or Element Materials Technology
- **Reference products:** Ridge Wallet (Santa Monica, CA) and Dango Products (Indianapolis, IN) — both excellent examples of this form factor

---

## Summary Notes

**Domestic manufacturing priority:** Where possible, all sourcing above favors US-based manufacturers for ITAR compliance, supply chain resilience, and BAA/Buy American Act alignment.

**Volume thresholds:** Most OEM relationships above require minimum order quantities of 250–500 units to unlock private-label programs. Initial market testing may require purchasing finished goods from the same suppliers under retail programs.

**Regulatory considerations:**
- Edged weapons (Durendal): State-by-state blade length laws; federal switchblade regulations; export controls for certain geometries
- Breaching tools (Aries): No specific federal licensing; some jurisdictions restrict sale of breaching tools to law enforcement/military only — verify per state
- Medical (Balor): FDA registration requirements for Class I/II devices; TCCC curriculum alignment recommended
- Optics/thermal (Weathertop): ITAR controls on thermal imagers with NETD <20mK or resolution >640x480; EAR for most commercial-grade optics
- Analytics software (Aeglos Analytics): FedRAMP authorization path if cloud-adjacent; ATO (Authority to Operate) for classified environments

**Recommended first products to market:** Balor IFAK (lowest regulatory burden, high demand), Corvus Multi-Tool (Leatherman OEM, fastest path to product), Durendal MK-1 Fixed Blade (ESEE OEM relationship).
