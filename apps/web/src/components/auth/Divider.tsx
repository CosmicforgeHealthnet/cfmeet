import React from "react";

type DividerProps = {
  text?: string;
};

const Divider = ({ text = "OR" }: DividerProps) => {
  return (
    <div className="relative my-2 sm:my-3">
      {/* Lines */}
      <div className="absolute inset-0 flex items-center">
        <div className="w-full border-t border-[#029CD4]/30"></div>
      </div>

      {/* Text */}
      <div className="relative flex justify-center">
        <span className="bg-white px-3 sm:px-4 text-[12px] sm:text-[16px] font-bold tracking-widest text-[#029CD44D]">
          {text}
        </span>
      </div>
    </div>
  );
};

export default Divider;
